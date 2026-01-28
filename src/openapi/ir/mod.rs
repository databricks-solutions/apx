//! Intermediate Representation for OpenAPI to TypeScript code generation.
//!
//! This module defines a two-layer IR:
//! 1. API-level IR: Normalized operations, parameters, hooks (OpenAPI-agnostic)
//! 2. TypeScript IR: Types and expressions for code generation
//!
//! The separation allows:
//! - All OpenAPI corner cases resolved in normalization
//! - Emitters become mechanical printers with no branching logic
//!
//! ## Module Structure
//!
//! - `types`: TypeScript IR (TsType, TsExpr, etc.)
//! - `api`: API-level IR (OperationIR, ParamsIR, etc.)
//! - `normalize`: OpenAPI spec to API IR conversion
//! - `printer`: API IR to TypeScript code
//! - `utils`: Common utilities shared across modules

mod api;
mod normalize;
mod printer;
mod types;
pub mod utils;

// Re-export the main entry points
pub use normalize::normalize_spec;
pub use printer::print_module;
