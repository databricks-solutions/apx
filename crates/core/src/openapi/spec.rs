//! OpenAPI specification structs for serde deserialization.
//!
//! This module defines a minimal subset of the OpenAPI 3.1 spec that we need
//! to parse FastAPI-generated schemas and produce TypeScript code.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Root OpenAPI specification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenApiSpec {
    pub paths: HashMap<String, PathItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub components: Option<Components>,
}

/// Components section containing reusable schemas.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Components {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schemas: Option<HashMap<String, Schema>>,
}

/// A path item containing operations for different HTTP methods.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathItem {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub get: Option<Operation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub post: Option<Operation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub put: Option<Operation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub patch: Option<Operation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delete: Option<Operation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head: Option<Operation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<Operation>,
    /// Path-level parameters shared by all operations.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parameters: Option<Vec<Parameter>>,
}

/// An API operation (endpoint).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Operation {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parameters: Option<Vec<Parameter>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_body: Option<RequestBody>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub responses: HashMap<String, Response>,
}

/// A parameter (query, path, or header).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Parameter {
    pub name: String,
    #[serde(rename = "in")]
    pub location: String,
    #[serde(default)]
    pub required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema: Option<Schema>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// A request body definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestBody {
    #[serde(default)]
    pub required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<HashMap<String, MediaType>>,
}

/// A response definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<HashMap<String, MediaType>>,
}

/// Media type content (e.g., application/json).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaType {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema: Option<Schema>,
}

/// JSON Schema definition used in OpenAPI.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Schema {
    /// The type of the schema (string, number, integer, boolean, object, array).
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub schema_type: Option<SchemaType>,

    /// Reference to another schema.
    #[serde(rename = "$ref", skip_serializing_if = "Option::is_none")]
    pub ref_path: Option<String>,

    /// Properties for object types.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub properties: Option<HashMap<String, Schema>>,

    /// Required property names for object types.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required: Option<Vec<String>>,

    /// Item schema for array types.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub items: Option<Box<Schema>>,

    /// Enum values (can be strings, integers, floats, booleans, or null).
    #[serde(rename = "enum", skip_serializing_if = "Option::is_none")]
    pub enum_values: Option<Vec<EnumValue>>,

    /// Union type (any of these schemas).
    #[serde(rename = "anyOf", skip_serializing_if = "Option::is_none")]
    pub any_of: Option<Vec<Schema>>,

    /// Union type (exactly one of these schemas).
    #[serde(rename = "oneOf", skip_serializing_if = "Option::is_none")]
    pub one_of: Option<Vec<Schema>>,

    /// Intersection type (all of these schemas combined).
    #[serde(rename = "allOf", skip_serializing_if = "Option::is_none")]
    pub all_of: Option<Vec<Schema>>,

    /// Additional properties for object types (for Record/dict types).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub additional_properties: Option<AdditionalProperties>,

    /// Discriminator for polymorphic oneOf schemas.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub discriminator: Option<Discriminator>,

    /// Format hint (e.g., date-time, uuid).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,

    /// Title of the schema.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,

    /// Description of the schema.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// Example value for the schema.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub example: Option<serde_json::Value>,

    // --- Validation keywords (parsed but not directly emitted as types) ---
    /// Constant value - schema matches only this exact value.
    #[serde(rename = "const", skip_serializing_if = "Option::is_none")]
    pub const_value: Option<serde_json::Value>,

    /// Default value for the schema.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<serde_json::Value>,

    /// OpenAPI 3.0 nullable flag (3.1 uses type arrays instead).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nullable: Option<bool>,

    /// Regex pattern for string validation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,

    /// Minimum value for numbers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minimum: Option<f64>,

    /// Maximum value for numbers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub maximum: Option<f64>,

    /// Exclusive minimum value for numbers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exclusive_minimum: Option<f64>,

    /// Exclusive maximum value for numbers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exclusive_maximum: Option<f64>,

    /// Minimum length for strings.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_length: Option<u64>,

    /// Maximum length for strings.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_length: Option<u64>,

    /// Minimum items for arrays.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_items: Option<u64>,

    /// Maximum items for arrays.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_items: Option<u64>,
}

/// Enum value can be string, integer, float, boolean, or null.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum EnumValue {
    String(String),
    Integer(i64),
    Float(f64),
    Bool(bool),
    Null,
}

/// Discriminator for polymorphic schemas (oneOf/anyOf).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Discriminator {
    /// The property name that contains the discriminator value.
    pub property_name: String,
    /// Optional mapping from discriminator values to schema refs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mapping: Option<HashMap<String, String>>,
}

/// Schema type can be a single type or an array of types (for nullable).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SchemaType {
    Single(String),
    Multiple(Vec<String>),
}

/// Additional properties can be a boolean or a schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// Check if this schema is nullable (contains null in anyOf, type array, or nullable flag).
    pub fn is_nullable(&self) -> bool {
        // Check OpenAPI 3.0 nullable flag
        if self.nullable == Some(true) {
            return true;
        }

        // Check anyOf for null type
        if let Some(any_of) = &self.any_of {
            for schema in any_of {
                if let Some(SchemaType::Single(t)) = &schema.schema_type
                    && t == "null"
                {
                    return true;
                }
            }
        }

        // Check type array for null
        if let Some(SchemaType::Multiple(types)) = &self.schema_type
            && types.iter().any(|t| t == "null")
        {
            return true;
        }

        false
    }

    /// Get the non-null schema from an anyOf that includes null.
    pub fn unwrap_nullable(&self) -> Option<&Schema> {
        if let Some(any_of) = &self.any_of {
            for schema in any_of {
                if let Some(SchemaType::Single(t)) = &schema.schema_type
                    && t != "null"
                {
                    return Some(schema);
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
