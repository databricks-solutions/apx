//! TypeScript code emitter for OpenAPI specifications.
//!
//! This module is a thin wrapper around the IR-based code generation.
//! The pipeline is:
//! 1. Parse: OpenAPI JSON -> OpenApiSpec
//! 2. Normalize: OpenApiSpec -> ApiIR (all OpenAPI logic resolved)
//! 3. Codegen: ApiIR -> swc_ecma_ast::Module
//! 4. Emit: Module -> String (via SWC's Emitter)

use std::rc::Rc;

use swc_common::SourceMap;
use swc_common::sync::Lrc;
use swc_ecma_ast::Module;
use swc_ecma_codegen::{Config, Emitter as SwcEmitter, text_writer::JsWriter};

use crate::openapi::ir::{codegen_module, normalize_spec};
use crate::openapi::spec::OpenApiSpec;

/// Generate TypeScript code from an OpenAPI JSON string.
pub fn generate(openapi_json: &str) -> Result<String, String> {
    // Parse OpenAPI spec
    let spec = OpenApiSpec::from_json(openapi_json)?;

    // Normalize to API IR (all OpenAPI logic resolved here)
    let api_ir = normalize_spec(&spec)?;

    // Generate SWC AST
    let module = codegen_module(&api_ir);

    // Emit to string
    emit_module(&module)
}

/// Emit a SWC Module to a TypeScript string.
fn emit_module(module: &Module) -> Result<String, String> {
    let cm: Lrc<SourceMap> = Rc::default();
    let mut buf = vec![];
    {
        let mut emitter = SwcEmitter {
            cfg: Config::default().with_ascii_only(false),
            cm: cm.clone(),
            comments: None,
            wr: JsWriter::new(cm, "\n", &mut buf, None),
        };
        emitter
            .emit_module(module)
            .map_err(|e| format!("SWC emit error: {e}"))?;
    }
    let code = String::from_utf8(buf).map_err(|e| format!("UTF-8 error: {e}"))?;
    Ok(code)
}
