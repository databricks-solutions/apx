//! API-level IR for normalized operations.
//!
//! This module defines the intermediate representation for API operations:
//! - OperationIR: Normalized HTTP operations
//! - ParamsIR: Path and query parameters
//! - FetchIR: Fetch function representation
//! - HookIR: React Query hook representation

// Allow dead code for IR types that are part of the design but not yet fully utilized.
#![allow(dead_code)]

use super::types::{TsTypeDef, TypeRef};

/// HTTP method
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpMethod {
    Get,
    Post,
    Put,
    Patch,
    Delete,
}

impl HttpMethod {
    pub fn as_str(&self) -> &'static str {
        match self {
            HttpMethod::Get => "GET",
            HttpMethod::Post => "POST",
            HttpMethod::Put => "PUT",
            HttpMethod::Patch => "PATCH",
            HttpMethod::Delete => "DELETE",
        }
    }

    pub fn is_query(&self) -> bool {
        matches!(self, HttpMethod::Get)
    }
}

/// Operation kind (query vs mutation)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationKind {
    /// GET requests - read operations
    Query,
    /// POST, PUT, PATCH, DELETE - write operations
    Mutation,
}

/// Normalized API operation
#[derive(Debug, Clone)]
pub struct OperationIR {
    /// Sanitized TypeScript identifier (e.g., "listItems")
    pub name: String,
    /// Query or mutation
    pub kind: OperationKind,
    /// URL path (e.g., "/items/{itemId}")
    pub path: String,
    /// HTTP method
    pub method: HttpMethod,

    /// Normalized parameters (None = no params)
    pub params: Option<ParamsIR>,
    /// Request body (None = no body)
    pub body: Option<BodyIR>,
    /// Response information
    pub response: ResponseIR,

    /// Precomputed fetch function IR
    pub fetch: FetchIR,
    /// Precomputed hooks (useQuery, useSuspenseQuery, or useMutation)
    pub hooks: Vec<HookIR>,
    /// Query key function (only for queries)
    pub query_key: Option<QueryKeyIR>,
}

/// Parameter location
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamLocation {
    Path,
    Query,
    Header,
}

/// Single parameter definition
#[derive(Debug, Clone)]
pub struct ParamIR {
    /// TypeScript-safe identifier
    pub name: String,
    /// Original name from spec (for URL building)
    pub original_name: String,
    /// Parameter type
    pub ty: TypeRef,
    /// Whether the parameter is required
    pub required: bool,
    /// Where the parameter appears
    pub location: ParamLocation,
}

/// Parameters interface definition
#[derive(Debug, Clone)]
pub struct ParamsIR {
    /// Type name (e.g., "ListItemsParams")
    pub type_name: String,
    /// Parameter fields
    pub fields: Vec<ParamIR>,
}

/// Fetch function IR
#[derive(Debug, Clone)]
pub struct FetchIR {
    /// Function name
    pub fn_name: String,
    /// Function arguments
    pub args: Vec<FetchArgIR>,
    /// Response information (type, content type, void status)
    pub response: ResponseIR,
    /// URL construction
    pub url: UrlIR,
    /// Request body (if any)
    pub body: Option<BodyIR>,
    /// HTTP method
    pub method: HttpMethod,
    /// Header parameters to include in fetch headers
    pub header_params: Vec<ParamIR>,
}

/// Fetch function argument
#[derive(Debug, Clone)]
pub enum FetchArgIR {
    /// Parameters argument
    Params { ty: TypeRef, optional: bool },
    /// Request body argument
    Body {
        ty: TypeRef,
        content_type: BodyContentType,
    },
    /// RequestInit options
    Options,
}

/// URL construction IR
#[derive(Debug, Clone)]
pub struct UrlIR {
    /// URL template parts
    pub template: Vec<UrlPart>,
    /// Query parameters to append
    pub query_params: Vec<ParamIR>,
}

/// URL template part
#[derive(Debug, Clone)]
pub enum UrlPart {
    /// Static string
    Static(String),
    /// Parameter interpolation
    Param(String),
}

/// Response content type determines how to parse the response
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponseContentType {
    /// JSON response - use res.json()
    Json,
    /// Plain text response - use res.text()
    Text,
    /// Binary/blob response - use res.blob()
    Blob,
    /// Unknown content type - return Response directly
    Unknown,
}

/// Request body content type determines how to serialize the body
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyContentType {
    /// JSON body - use JSON.stringify()
    Json,
    /// multipart/form-data - pass FormData directly
    FormData,
    /// application/x-www-form-urlencoded - use URLSearchParams
    UrlEncoded,
}

/// Request body IR
#[derive(Debug, Clone)]
pub struct BodyIR {
    pub ty: TypeRef,
    pub content_type: BodyContentType,
}

/// Response IR with content type info
#[derive(Debug, Clone)]
pub struct ResponseIR {
    /// The response type
    pub ty: TypeRef,
    /// How to parse the response
    pub content_type: ResponseContentType,
    /// Whether a void status (204) exists alongside content response
    pub has_void_status: bool,
}

/// Hook kind
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookKind {
    Query,
    SuspenseQuery,
    Mutation,
}

/// React Query hook IR
#[derive(Debug, Clone)]
pub struct HookIR {
    /// Hook name (e.g., "useListItems")
    pub name: String,
    /// Hook kind
    pub kind: HookKind,
    /// Response type
    pub response_type: TypeRef,
    /// Variables type (mutation vars or query params)
    pub vars_type: Option<TypeRef>,
    /// Reference to fetch function
    pub fetch_fn: String,
    /// Query key function (for queries)
    pub query_key_fn: Option<String>,
    /// Whether params are required (has required path/query params)
    pub params_required: bool,
    /// Response content type (for determining actual TS type: Blob, string, etc.)
    pub response_content_type: ResponseContentType,
    /// Whether a 204 void status exists alongside content response
    pub response_has_void_status: bool,
    /// For mutations: whether body argument comes before params in fetch function
    pub body_before_params: bool,
}

/// Query key function IR
#[derive(Debug, Clone)]
pub struct QueryKeyIR {
    /// Function name (e.g., "listItemsKey")
    pub fn_name: String,
    /// Base key string (e.g., "/items")
    pub base_key: String,
    /// Parameters type (if any)
    pub params_type: Option<TypeRef>,
}

/// Normalized API specification
#[derive(Debug)]
pub struct ApiIR {
    /// All operations
    pub operations: Vec<OperationIR>,
    /// Component schemas as type definitions
    pub types: Vec<TsTypeDef>,
    /// Whether the spec has queries
    pub has_queries: bool,
    /// Whether the spec has mutations
    pub has_mutations: bool,
}
