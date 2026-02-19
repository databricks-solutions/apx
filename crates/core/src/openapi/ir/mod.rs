//! Intermediate Representation for OpenAPI to TypeScript code generation.
//!
//! This module defines a three-layer architecture:
//! 1. API-level IR: Normalized operations, parameters, hooks (OpenAPI-agnostic)
//! 2. Type-only IR: Domain types (TsType, TsProp, TsTypeDef, TypeRef)
//! 3. SWC codegen: API IR → swc_ecma_ast::Module (via builder helpers)
//!
//! The separation allows:
//! - All OpenAPI corner cases resolved in normalization
//! - Code generation builds a standard SWC AST (testable, correct emission)
//! - Emission is handled by SWC's own codegen
//!
//! ## Module Structure
//!
//! - `types`: Type-only IR (TsType, TsProp, TsTypeDef, TypeRef, TsLiteral)
//! - `api`: API-level IR (OperationIR, ParamsIR, FetchIR, HookIR)
//! - `normalize`: OpenAPI spec -> API IR conversion
//! - `codegen`: API IR -> swc_ecma_ast::Module
//! - `builders`: Ergonomic helpers for SWC AST construction
//! - `utils`: Common utilities shared across modules

mod api;
#[macro_use]
pub mod builders;
mod codegen;
mod normalize;
mod types;
pub mod utils;

// Re-export the main entry points
pub use codegen::codegen_module;
pub use normalize::normalize_spec;
