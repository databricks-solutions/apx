use serde::{Deserialize, Serialize};

use crate::client::DatabricksClient;
use crate::error::Result;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct User {
    pub id: String,
    pub user_name: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub emails: Vec<UserEmail>,
    #[serde(default)]
    pub active: Option<bool>,
    #[serde(default)]
    pub name: Option<UserName>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserEmail {
    pub value: String,
    #[serde(default)]
    pub primary: Option<bool>,
    #[serde(rename = "type", default)]
    pub email_type: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserName {
    #[serde(default)]
    pub given_name: Option<String>,
    #[serde(default)]
    pub family_name: Option<String>,
}

pub struct CurrentUserApi<'a> {
    client: &'a DatabricksClient,
}

impl<'a> CurrentUserApi<'a> {
    pub(crate) fn new(client: &'a DatabricksClient) -> Self {
        Self { client }
    }

    pub async fn me(&self) -> Result<User> {
        self.client.get("/api/2.0/preview/scim/v2/Me").await
    }
}
