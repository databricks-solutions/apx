//! Intermediate Representation for OpenAPI to TypeScript code generation.
//!
//! This module defines a three-layer architecture:
//! 1. API-level IR: Normalized operations, parameters, hooks (OpenAPI-agnostic)
//! 2. TypeScript AST IR: Types, expressions, statements, functions
//! 3. Emission: AST to TypeScript code strings via the `Emit` trait
//!
//! The separation allows:
//! - All OpenAPI corner cases resolved in normalization
//! - Code generation builds structured AST (testable)
//! - Emission is purely mechanical string building
//!
//! ## Module Structure
//!
//! - `types`: TypeScript AST IR (TsType, TsExpr, TsStmt, TsFunction, TsModule)
//! - `api`: API-level IR (OperationIR, ParamsIR, FetchIR, HookIR)
//! - `normalize`: OpenAPI spec -> API IR conversion
//! - `codegen`: API IR -> TypeScript AST
//! - `emit`: TypeScript AST -> code strings (via Emit trait)
//! - `utils`: Common utilities shared across modules

mod api;
mod codegen;
mod emit;
mod normalize;
mod types;
pub mod utils;

// Re-export the main entry points
pub use codegen::codegen_module;
pub use emit::Emit;
pub use normalize::normalize_spec;
