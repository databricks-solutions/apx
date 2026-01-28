//! TypeScript code emitter for OpenAPI specifications.
//!
//! This module is now a thin wrapper around the IR-based code generation.
//! All the heavy lifting is done in the `ir` module:
//! - Normalization: OpenAPI spec -> API IR (all corner cases resolved)
//! - Printing: API IR -> TypeScript code (mechanical, no branching)

use crate::openapi::ir::{normalize_spec, print_module};
use crate::openapi::spec::OpenApiSpec;

/// Generate TypeScript code from an OpenAPI JSON string.
pub fn generate(openapi_json: &str) -> Result<String, String> {
    // Parse OpenAPI spec
    let spec = OpenApiSpec::from_json(openapi_json)?;

    // Normalize to IR (all OpenAPI logic resolved here)
    let api_ir = normalize_spec(&spec)?;

    // Print to TypeScript (mechanical, no branching)
    Ok(print_module(&api_ir))
}
