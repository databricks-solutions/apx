use serde::{Deserialize, Serialize};

use crate::client::DatabricksClient;
use crate::error::Result;

/// A Databricks user as returned by the SCIM `/Me` endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct User {
    /// Unique user identifier.
    pub id: String,
    /// Login username (typically an email address).
    pub user_name: String,
    /// Human-readable display name.
    #[serde(default)]
    pub display_name: Option<String>,
    /// Email addresses associated with the user.
    #[serde(default)]
    pub emails: Vec<UserEmail>,
    /// Whether the user account is active.
    #[serde(default)]
    pub active: Option<bool>,
    /// Structured name (given/family).
    #[serde(default)]
    pub name: Option<UserName>,
}

/// An email address entry from the SCIM user record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserEmail {
    /// The email address.
    pub value: String,
    /// Whether this is the primary email.
    #[serde(default)]
    pub primary: Option<bool>,
    /// Email type (e.g. `"work"`).
    #[serde(rename = "type", default)]
    pub email_type: Option<String>,
}

/// Structured name from the SCIM user record.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserName {
    /// First / given name.
    #[serde(default)]
    pub given_name: Option<String>,
    /// Last / family name.
    #[serde(default)]
    pub family_name: Option<String>,
}

/// API handle for current-user (SCIM `/Me`) operations.
#[derive(Debug)]
pub struct CurrentUserApi<'a> {
    client: &'a DatabricksClient,
}

impl<'a> CurrentUserApi<'a> {
    pub(crate) const fn new(client: &'a DatabricksClient) -> Self {
        Self { client }
    }

    /// Fetch the current user's profile.
    ///
    /// # Errors
    ///
    /// Returns an error if the HTTP request fails or the response cannot be deserialized.
    pub async fn me(&self) -> Result<User> {
        self.client.get("/api/2.0/preview/scim/v2/Me").await
    }
}
