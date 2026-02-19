use std::sync::Arc;

use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::sync::RwLock;
use tracing::debug;

use crate::api::current_user::CurrentUserApi;
use crate::auth::{CachedToken, acquire_token};
use crate::config::{DatabricksConfig, resolve_config};
use crate::error::{DatabricksError, Result};
use crate::useragent::UserAgent;

struct Inner {
    config: DatabricksConfig,
    http: reqwest::Client,
    cached_token: RwLock<Option<CachedToken>>,
}

impl std::fmt::Debug for Inner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Inner")
            .field("profile", &self.config.profile)
            .field("host", &self.config.host)
            .finish()
    }
}

#[derive(Debug, Clone)]
pub struct DatabricksClient {
    inner: Arc<Inner>,
}

impl DatabricksClient {
    /// Create a new client by resolving the given profile from `~/.databrickscfg`.
    /// Does not eagerly fetch a token.
    pub async fn new(profile: &str) -> Result<Self> {
        let config = resolve_config(profile)?;
        Ok(Self::from_config(config))
    }

    /// Create a new client with explicit product info for the User-Agent header.
    pub async fn with_product(profile: &str, product: &str, product_version: &str) -> Result<Self> {
        let mut config = resolve_config(profile)?;
        config.product = Some(product.to_string());
        config.product_version = Some(product_version.to_string());
        Ok(Self::from_config(config))
    }

    /// Create a client from an already-resolved config.
    pub fn from_config(config: DatabricksConfig) -> Self {
        let product = config.product.as_deref().unwrap_or("unknown");
        let product_version = config.product_version.as_deref().unwrap_or("0.0.0");

        let ua = UserAgent::new(product, product_version).with_auth("databricks-cli");

        let http = reqwest::Client::builder()
            .user_agent(ua.to_string())
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        Self {
            inner: Arc::new(Inner {
                config,
                http,
                cached_token: RwLock::new(None),
            }),
        }
    }

    /// Get a valid access token, refreshing via the CLI if needed.
    async fn get_token(&self) -> Result<String> {
        // Fast path: read lock
        {
            let guard = self.inner.cached_token.read().await;
            if let Some(ref token) = *guard
                && token.is_valid()
            {
                return Ok(token.access_token.clone());
            }
        }

        // Slow path: write lock + acquire
        let mut guard = self.inner.cached_token.write().await;

        // Double-check after acquiring write lock
        if let Some(ref token) = *guard
            && token.is_valid()
        {
            return Ok(token.access_token.clone());
        }

        debug!(profile = %self.inner.config.profile, "Token expired or missing, acquiring new token");
        let new_token = acquire_token(&self.inner.config.profile).await?;
        let access_token = new_token.access_token.clone();
        *guard = Some(new_token);
        Ok(access_token)
    }

    /// Perform an authenticated GET request and deserialize the JSON response.
    pub(crate) async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let token = self.get_token().await?;
        let url = format!("{}{}", self.inner.config.host, path);

        let response = self.inner.http.get(&url).bearer_auth(&token).send().await?;

        handle_response(response).await
    }

    /// Perform an authenticated POST request and deserialize the JSON response.
    #[allow(dead_code)]
    pub(crate) async fn post<B: Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T> {
        let token = self.get_token().await?;
        let url = format!("{}{}", self.inner.config.host, path);

        let response = self
            .inner
            .http
            .post(&url)
            .bearer_auth(&token)
            .json(body)
            .send()
            .await?;

        handle_response(response).await
    }

    /// Get a raw access token for proxy forwarding.
    pub async fn access_token(&self) -> Result<String> {
        self.get_token().await
    }

    pub fn host(&self) -> &str {
        &self.inner.config.host
    }

    pub fn profile(&self) -> &str {
        &self.inner.config.profile
    }

    pub fn current_user(&self) -> CurrentUserApi<'_> {
        CurrentUserApi::new(self)
    }
}

async fn handle_response<T: DeserializeOwned>(response: reqwest::Response) -> Result<T> {
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.ok();
        let message = body.as_deref().unwrap_or("(no response body)").to_string();
        return Err(DatabricksError::Api {
            status: status.as_u16(),
            message,
            body: None,
        });
    }

    let body = response.text().await?;
    serde_json::from_str(&body).map_err(Into::into)
}
