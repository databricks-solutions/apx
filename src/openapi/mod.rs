//! OpenAPI to TypeScript code generator.
//!
//! This module parses OpenAPI 3.1 specifications and generates TypeScript code with:
//! - Type definitions from component schemas
//! - Fetch-based API client functions
//! - React Query hooks (useQuery, useSuspenseQuery, useMutation)

mod emitter;
mod spec;

pub use emitter::generate;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_from_openapi_json() {
        let openapi_json = include_str!("../../experiments/gen/openapi.json");
        let result = generate(openapi_json);
        assert!(result.is_ok(), "Generation failed: {:?}", result.err());

        let ts_code = result.unwrap();

        // Print generated code for debugging
        println!("=== GENERATED CODE ===\n{}\n=== END ===", ts_code);

        // Verify imports
        assert!(ts_code.contains("import {"), "Missing imports");
        assert!(ts_code.contains("useQuery"), "Missing useQuery import");
        assert!(ts_code.contains("useSuspenseQuery"), "Missing useSuspenseQuery import");
        assert!(ts_code.contains("useMutation"), "Missing useMutation import");

        // Verify types are generated
        assert!(ts_code.contains("export interface Item {"), "Missing Item interface");
        assert!(ts_code.contains("export interface CreateItemInput {"), "Missing CreateItemInput interface");
        assert!(ts_code.contains("export interface PaginatedItems {"), "Missing PaginatedItems interface");
        assert!(ts_code.contains("export interface ErrorResponse {"), "Missing ErrorResponse interface");

        // Verify fetch functions
        assert!(ts_code.contains("export const listItems = async"), "Missing listItems function");
        assert!(ts_code.contains("export const createItem = async"), "Missing createItem function");
        assert!(ts_code.contains("export const getItem = async"), "Missing getItem function");
        assert!(ts_code.contains("export const deleteItem = async"), "Missing deleteItem function");

        // Verify hooks
        assert!(ts_code.contains("export function useListItems"), "Missing useListItems hook");
        assert!(ts_code.contains("export function useListItemsSuspense"), "Missing useListItemsSuspense hook");
        assert!(ts_code.contains("export function useCreateItem"), "Missing useCreateItem hook");
        assert!(ts_code.contains("export function useGetItem"), "Missing useGetItem hook");
        assert!(ts_code.contains("export function useDeleteItem"), "Missing useDeleteItem hook");

        println!("Generated TypeScript code length: {} bytes", ts_code.len());
    }
}
