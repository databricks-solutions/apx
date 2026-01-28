//! TypeScript code emitter for OpenAPI specifications.
//!
//! This module is a thin wrapper around the IR-based code generation.
//! The pipeline is:
//! 1. Parse: OpenAPI JSON -> OpenApiSpec
//! 2. Normalize: OpenApiSpec -> ApiIR (all OpenAPI logic resolved)
//! 3. Codegen: ApiIR -> TsModule (TypeScript AST)
//! 4. Emit: TsModule -> String (via Emit trait)

use crate::openapi::ir::{codegen_module, normalize_spec, Emit};
use crate::openapi::spec::OpenApiSpec;

/// Generate TypeScript code from an OpenAPI JSON string.
pub fn generate(openapi_json: &str) -> Result<String, String> {
    // Parse OpenAPI spec
    let spec = OpenApiSpec::from_json(openapi_json)?;

    // Normalize to API IR (all OpenAPI logic resolved here)
    let api_ir = normalize_spec(&spec)?;

    // Generate TypeScript AST and emit to string
    Ok(codegen_module(&api_ir).emit())
}
