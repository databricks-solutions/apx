//! OpenAPI specification structs for serde deserialization.
//!
//! This module defines a minimal subset of the OpenAPI 3.1 spec that we need
//! to parse FastAPI-generated schemas and produce TypeScript code.

// Allow unused fields that are part of OpenAPI spec for completeness
#![allow(dead_code)]

use serde::Deserialize;
use std::collections::HashMap;

/// Root OpenAPI specification.
#[derive(Debug, Deserialize)]
pub struct OpenApiSpec {
    pub paths: HashMap<String, PathItem>,
    pub components: Option<Components>,
}

/// Components section containing reusable schemas.
#[derive(Debug, Deserialize)]
pub struct Components {
    pub schemas: Option<HashMap<String, Schema>>,
}

/// A path item containing operations for different HTTP methods.
#[derive(Debug, Deserialize)]
pub struct PathItem {
    pub get: Option<Operation>,
    pub post: Option<Operation>,
    pub put: Option<Operation>,
    pub patch: Option<Operation>,
    pub delete: Option<Operation>,
    /// Path-level parameters shared by all operations.
    pub parameters: Option<Vec<Parameter>>,
}

/// An API operation (endpoint).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Operation {
    pub operation_id: Option<String>,
    pub summary: Option<String>,
    pub parameters: Option<Vec<Parameter>>,
    pub request_body: Option<RequestBody>,
    #[serde(default)]
    pub responses: HashMap<String, Response>,
}

/// A parameter (query, path, or header).
#[derive(Debug, Deserialize)]
pub struct Parameter {
    pub name: String,
    #[serde(rename = "in")]
    pub location: String,
    #[serde(default)]
    pub required: bool,
    pub schema: Option<Schema>,
}

/// A request body definition.
#[derive(Debug, Deserialize)]
pub struct RequestBody {
    #[serde(default)]
    pub required: bool,
    pub content: Option<HashMap<String, MediaType>>,
}

/// A response definition.
#[derive(Debug, Deserialize)]
pub struct Response {
    pub description: Option<String>,
    pub content: Option<HashMap<String, MediaType>>,
}

/// Media type content (e.g., application/json).
#[derive(Debug, Deserialize)]
pub struct MediaType {
    pub schema: Option<Schema>,
}

/// JSON Schema definition used in OpenAPI.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Schema {
    /// The type of the schema (string, number, integer, boolean, object, array).
    #[serde(rename = "type")]
    pub schema_type: Option<SchemaType>,

    /// Reference to another schema.
    #[serde(rename = "$ref")]
    pub ref_path: Option<String>,

    /// Properties for object types.
    pub properties: Option<HashMap<String, Schema>>,

    /// Required property names for object types.
    pub required: Option<Vec<String>>,

    /// Item schema for array types.
    pub items: Option<Box<Schema>>,

    /// Enum values for string enums.
    #[serde(rename = "enum")]
    pub enum_values: Option<Vec<String>>,

    /// Union type (any of these schemas).
    #[serde(rename = "anyOf")]
    pub any_of: Option<Vec<Schema>>,

    /// Union type (exactly one of these schemas).
    #[serde(rename = "oneOf")]
    pub one_of: Option<Vec<Schema>>,

    /// Additional properties for object types (for Record/dict types).
    pub additional_properties: Option<AdditionalProperties>,

    /// Format hint (e.g., date-time, uuid).
    pub format: Option<String>,
}

/// Schema type can be a single type or an array of types (for nullable).
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum SchemaType {
    Single(String),
    Multiple(Vec<String>),
}

/// Additional properties can be a boolean or a schema.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum AdditionalProperties {
    Bool(bool),
    Schema(Box<Schema>),
}

impl OpenApiSpec {
    /// Parse an OpenAPI spec from a JSON string.
    pub fn from_json(json: &str) -> Result<Self, String> {
        serde_json::from_str(json).map_err(|e| format!("Failed to parse OpenAPI spec: {e}"))
    }
}

impl Schema {
    /// Check if this schema is nullable (contains null in anyOf or type array).
    pub fn is_nullable(&self) -> bool {
        // Check anyOf for null type
        if let Some(any_of) = &self.any_of {
            for schema in any_of {
                if let Some(SchemaType::Single(t)) = &schema.schema_type {
                    if t == "null" {
                        return true;
                    }
                }
            }
        }

        // Check type array for null
        if let Some(SchemaType::Multiple(types)) = &self.schema_type {
            if types.iter().any(|t| t == "null") {
                return true;
            }
        }

        false
    }

    /// Get the non-null schema from an anyOf that includes null.
    pub fn unwrap_nullable(&self) -> Option<&Schema> {
        if let Some(any_of) = &self.any_of {
            for schema in any_of {
                if let Some(SchemaType::Single(t)) = &schema.schema_type {
                    if t != "null" {
                        return Some(schema);
                    }
                }
                // If it's a ref or complex schema, return it
                if schema.ref_path.is_some() || schema.properties.is_some() {
                    return Some(schema);
                }
            }
        }
        None
    }
}
