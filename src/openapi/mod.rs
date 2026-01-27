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

    const TEST_OPENAPI_JSON: &str = r##"{
  "openapi": "3.1.0",
  "info": { "title": "Test API", "version": "1.0.0" },
  "paths": {
    "/items": {
      "get": {
        "operationId": "listItems",
        "parameters": [
          { "name": "limit", "in": "query", "required": false, "schema": { "type": "integer" } },
          { "name": "cursor", "in": "query", "required": false, "schema": { "anyOf": [{ "type": "string" }, { "type": "null" }] } }
        ],
        "responses": {
          "200": { "description": "OK", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/PaginatedItems" } } } }
        }
      },
      "post": {
        "operationId": "createItem",
        "requestBody": { "required": true, "content": { "application/json": { "schema": { "$ref": "#/components/schemas/CreateItemInput" } } } },
        "responses": {
          "201": { "description": "Created", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/Item" } } } },
          "400": { "description": "Error", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ErrorResponse" } } } }
        }
      }
    },
    "/items/{itemId}": {
      "parameters": [{ "name": "itemId", "in": "path", "required": true, "schema": { "type": "string" } }],
      "get": {
        "operationId": "getItem",
        "responses": { "200": { "description": "OK", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/Item" } } } } }
      },
      "put": {
        "operationId": "replaceItem",
        "requestBody": { "required": true, "content": { "application/json": { "schema": { "$ref": "#/components/schemas/UpdateItemInput" } } } },
        "responses": { "200": { "description": "OK", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/Item" } } } } }
      },
      "patch": {
        "operationId": "patchItem",
        "requestBody": { "required": true, "content": { "application/json": { "schema": { "type": "object", "additionalProperties": true } } } },
        "responses": { "200": { "description": "OK", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/Item" } } } } }
      },
      "delete": {
        "operationId": "deleteItem",
        "responses": { "204": { "description": "Deleted" } }
      }
    },
    "/search": {
      "post": {
        "operationId": "search",
        "requestBody": { "required": true, "content": { "application/json": { "schema": { "oneOf": [{ "$ref": "#/components/schemas/TextSearch" }, { "$ref": "#/components/schemas/AdvancedSearch" }] } } } },
        "responses": { "200": { "description": "OK", "content": { "application/json": { "schema": { "type": "array", "items": { "$ref": "#/components/schemas/Item" } } } } } }
      }
    }
  },
  "components": {
    "schemas": {
      "Item": {
        "type": "object",
        "required": ["id", "name", "status"],
        "properties": {
          "id": { "type": "string" },
          "name": { "type": "string" },
          "status": { "type": "string", "enum": ["active", "archived"] },
          "tags": { "anyOf": [{ "type": "array", "items": { "type": "string" } }, { "type": "null" }] },
          "metadata": { "type": "object", "additionalProperties": { "type": "string" } },
          "createdAt": { "type": "string", "format": "date-time" }
        }
      },
      "CreateItemInput": {
        "type": "object",
        "required": ["name"],
        "properties": { "name": { "type": "string" }, "tags": { "type": "array", "items": { "type": "string" } } }
      },
      "UpdateItemInput": {
        "type": "object",
        "properties": { "name": { "type": "string" }, "tags": { "anyOf": [{ "type": "array", "items": { "type": "string" } }, { "type": "null" }] } }
      },
      "PaginatedItems": {
        "type": "object",
        "required": ["items"],
        "properties": { "items": { "type": "array", "items": { "$ref": "#/components/schemas/Item" } }, "nextCursor": { "anyOf": [{ "type": "string" }, { "type": "null" }] } }
      },
      "TextSearch": { "type": "object", "required": ["query"], "properties": { "query": { "type": "string" } } },
      "AdvancedSearch": {
        "type": "object",
        "required": ["filters"],
        "properties": { "filters": { "type": "object", "additionalProperties": { "anyOf": [{ "type": "string" }, { "type": "number" }, { "type": "boolean" }] } } }
      },
      "ErrorResponse": {
        "type": "object",
        "required": ["code", "message"],
        "properties": { "code": { "type": "string" }, "message": { "type": "string" }, "details": { "anyOf": [{ "type": "string" }, { "type": "array", "items": { "type": "string" } }, { "type": "null" }] } }
      }
    }
  }
}"##;

    #[test]
    fn test_generate_from_openapi_json() {
        let result = generate(TEST_OPENAPI_JSON);
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
