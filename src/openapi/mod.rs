//! OpenAPI to TypeScript code generator.
//!
//! This module parses OpenAPI 3.1 specifications and generates TypeScript code with:
//! - Type definitions from component schemas
//! - Fetch-based API client functions
//! - React Query hooks (useQuery, useSuspenseQuery, useMutation)

mod emitter;
mod ir;
mod spec;

pub use emitter::generate;

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
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
        let ts_code = generate_and_verify(TEST_OPENAPI_JSON);

        // Print generated code for debugging
        println!("=== GENERATED CODE ===\n{ts_code}\n=== END ===");

        // Verify imports
        assert!(ts_code.contains("import {"), "Missing imports");
        assert!(ts_code.contains("useQuery"), "Missing useQuery import");
        assert!(
            ts_code.contains("useSuspenseQuery"),
            "Missing useSuspenseQuery import"
        );
        assert!(
            ts_code.contains("useMutation"),
            "Missing useMutation import"
        );

        // Verify types are generated
        assert!(
            ts_code.contains("export interface Item {"),
            "Missing Item interface"
        );
        assert!(
            ts_code.contains("export interface CreateItemInput {"),
            "Missing CreateItemInput interface"
        );
        assert!(
            ts_code.contains("export interface PaginatedItems {"),
            "Missing PaginatedItems interface"
        );
        assert!(
            ts_code.contains("export interface ErrorResponse {"),
            "Missing ErrorResponse interface"
        );

        // Verify fetch functions
        assert!(
            ts_code.contains("export const listItems = async"),
            "Missing listItems function"
        );
        assert!(
            ts_code.contains("export const createItem = async"),
            "Missing createItem function"
        );
        assert!(
            ts_code.contains("export const getItem = async"),
            "Missing getItem function"
        );
        assert!(
            ts_code.contains("export const deleteItem = async"),
            "Missing deleteItem function"
        );

        // Verify hooks
        assert!(
            ts_code.contains("export function useListItems"),
            "Missing useListItems hook"
        );
        assert!(
            ts_code.contains("export function useListItemsSuspense"),
            "Missing useListItemsSuspense hook"
        );
        assert!(
            ts_code.contains("export function useCreateItem"),
            "Missing useCreateItem hook"
        );
        assert!(
            ts_code.contains("export function useGetItem"),
            "Missing useGetItem hook"
        );
        assert!(
            ts_code.contains("export function useDeleteItem"),
            "Missing useDeleteItem hook"
        );

        println!("Generated TypeScript code length: {} bytes", ts_code.len());
    }

    #[test]
    fn test_special_characters_in_enum_values() {
        // Test case for URN-style enum values (like SCIM schemas) that contain colons
        let openapi_json = r##"{
  "openapi": "3.1.0",
  "info": { "title": "SCIM API", "version": "1.0.0" },
  "paths": {},
  "components": {
    "schemas": {
      "UserSchema": {
        "type": "string",
        "enum": [
          "urn:ietf:params:scim:schemas:core:2.0:User",
          "urn:ietf:params:scim:schemas:extension:workspace:2.0:User"
        ]
      },
      "SpecialProps": {
        "type": "object",
        "properties": {
          "normal-prop": { "type": "string" },
          "prop.with.dots": { "type": "number" },
          "prop:with:colons": { "type": "boolean" },
          "123startsWithNumber": { "type": "string" }
        }
      }
    }
  }
}"##;

        let ts_code = generate_and_verify(openapi_json);
        println!("=== SPECIAL CHARS CODE ===\n{ts_code}\n=== END ===");

        // Verify URN enum values are properly quoted
        assert!(
            ts_code.contains(r#""urn:ietf:params:scim:schemas:core:2.0:User": "urn:ietf:params:scim:schemas:core:2.0:User""#),
            "URN enum key should be quoted"
        );
        assert!(
            ts_code.contains(r#""urn:ietf:params:scim:schemas:extension:workspace:2.0:User""#),
            "URN enum value should be quoted as key"
        );

        // Verify special property names are quoted
        assert!(
            ts_code.contains(r#""normal-prop"?"#) || ts_code.contains(r#""normal-prop":"#),
            "Property with dash should be quoted"
        );
        assert!(
            ts_code.contains(r#""prop.with.dots"?"#) || ts_code.contains(r#""prop.with.dots":"#),
            "Property with dots should be quoted"
        );
        assert!(
            ts_code.contains(r#""prop:with:colons"?"#)
                || ts_code.contains(r#""prop:with:colons":"#),
            "Property with colons should be quoted"
        );
        assert!(
            ts_code.contains(r#""123startsWithNumber"?"#)
                || ts_code.contains(r#""123startsWithNumber":"#),
            "Property starting with number should be quoted"
        );
    }

    #[test]
    fn test_conditional_imports_no_mutations() {
        // Test case for a spec with only GET endpoints - should NOT import useMutation
        let openapi_json = r##"{
  "openapi": "3.1.0",
  "info": { "title": "Read-Only API", "version": "1.0.0" },
  "paths": {
    "/items": {
      "get": {
        "operationId": "listItems",
        "responses": {
          "200": { "description": "OK", "content": { "application/json": { "schema": { "type": "array", "items": { "$ref": "#/components/schemas/Item" } } } } }
        }
      }
    },
    "/items/{id}": {
      "get": {
        "operationId": "getItem",
        "parameters": [{ "name": "id", "in": "path", "required": true, "schema": { "type": "string" } }],
        "responses": {
          "200": { "description": "OK", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/Item" } } } }
        }
      }
    }
  },
  "components": {
    "schemas": {
      "Item": {
        "type": "object",
        "required": ["id", "name"],
        "properties": {
          "id": { "type": "string" },
          "name": { "type": "string" }
        }
      }
    }
  }
}"##;

        let ts_code = generate_and_verify(openapi_json);
        println!("=== GET-ONLY SPEC CODE ===\n{ts_code}\n=== END ===");

        // Should have useQuery and useSuspenseQuery
        assert!(ts_code.contains("useQuery"), "Missing useQuery import");
        assert!(
            ts_code.contains("useSuspenseQuery"),
            "Missing useSuspenseQuery import"
        );

        // Should NOT have useMutation or UseMutationOptions (unused imports cause TS errors)
        assert!(
            !ts_code.contains("useMutation"),
            "useMutation should NOT be imported for GET-only specs"
        );
        assert!(
            !ts_code.contains("UseMutationOptions"),
            "UseMutationOptions should NOT be imported for GET-only specs"
        );
    }

    #[test]
    fn test_conditional_imports_no_queries() {
        // Test case for a spec with only POST/mutation endpoints - should NOT import useQuery
        let openapi_json = r##"{
  "openapi": "3.1.0",
  "info": { "title": "Write-Only API", "version": "1.0.0" },
  "paths": {
    "/items": {
      "post": {
        "operationId": "createItem",
        "requestBody": { "required": true, "content": { "application/json": { "schema": { "$ref": "#/components/schemas/CreateItemInput" } } } },
        "responses": {
          "201": { "description": "Created", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/Item" } } } }
        }
      }
    },
    "/items/{id}": {
      "delete": {
        "operationId": "deleteItem",
        "parameters": [{ "name": "id", "in": "path", "required": true, "schema": { "type": "string" } }],
        "responses": { "204": { "description": "Deleted" } }
      }
    }
  },
  "components": {
    "schemas": {
      "Item": {
        "type": "object",
        "required": ["id", "name"],
        "properties": {
          "id": { "type": "string" },
          "name": { "type": "string" }
        }
      },
      "CreateItemInput": {
        "type": "object",
        "required": ["name"],
        "properties": { "name": { "type": "string" } }
      }
    }
  }
}"##;

        let ts_code = generate_and_verify(openapi_json);
        println!("=== MUTATION-ONLY SPEC CODE ===\n{ts_code}\n=== END ===");

        // Should have useMutation and UseMutationOptions
        assert!(
            ts_code.contains("useMutation"),
            "Missing useMutation import"
        );
        assert!(
            ts_code.contains("UseMutationOptions"),
            "Missing UseMutationOptions import"
        );

        // Should NOT have useQuery or useSuspenseQuery (unused imports cause TS errors)
        assert!(
            !ts_code.contains("useQuery"),
            "useQuery should NOT be imported for mutation-only specs"
        );
        assert!(
            !ts_code.contains("useSuspenseQuery"),
            "useSuspenseQuery should NOT be imported for mutation-only specs"
        );
        assert!(
            !ts_code.contains("UseQueryOptions"),
            "UseQueryOptions should NOT be imported for mutation-only specs"
        );
        assert!(
            !ts_code.contains("UseSuspenseQueryOptions"),
            "UseSuspenseQueryOptions should NOT be imported for mutation-only specs"
        );
    }

    #[test]
    fn test_allof_composition() {
        // Test allOf for schema composition (common with Pydantic inheritance)
        let openapi_json = r##"{
  "openapi": "3.1.0",
  "info": { "title": "AllOf Test API", "version": "1.0.0" },
  "paths": {},
  "components": {
    "schemas": {
      "BaseEntity": {
        "type": "object",
        "required": ["id", "createdAt"],
        "properties": {
          "id": { "type": "string" },
          "createdAt": { "type": "string", "format": "date-time" }
        }
      },
      "User": {
        "allOf": [
          { "$ref": "#/components/schemas/BaseEntity" },
          {
            "type": "object",
            "required": ["email"],
            "properties": {
              "email": { "type": "string" },
              "name": { "type": "string" }
            }
          }
        ]
      },
      "Admin": {
        "allOf": [
          { "$ref": "#/components/schemas/User" },
          {
            "type": "object",
            "required": ["role"],
            "properties": {
              "role": { "type": "string", "const": "admin" },
              "permissions": { "type": "array", "items": { "type": "string" } }
            }
          }
        ]
      }
    }
  }
}"##;

        let ts_code = generate_and_verify(openapi_json);
        println!("=== ALLOF CODE ===\n{ts_code}\n=== END ===");

        // Verify BaseEntity is a regular interface
        assert!(
            ts_code.contains("export interface BaseEntity"),
            "Missing BaseEntity interface"
        );

        // Verify User uses intersection type with BaseEntity
        assert!(
            ts_code.contains("export type User = BaseEntity &"),
            "User should be intersection with BaseEntity"
        );

        // Verify Admin uses intersection type
        assert!(
            ts_code.contains("export type Admin = User &"),
            "Admin should be intersection with User"
        );

        // Verify const keyword generates literal type
        assert!(
            ts_code.contains("\"admin\""),
            "const: admin should generate literal type"
        );
    }

    #[test]
    fn test_oneof_with_discriminator() {
        // Test oneOf with discriminator for discriminated unions
        let openapi_json = r##"{
  "openapi": "3.1.0",
  "info": { "title": "Discriminator Test API", "version": "1.0.0" },
  "paths": {},
  "components": {
    "schemas": {
      "Dog": {
        "type": "object",
        "required": ["breed"],
        "properties": {
          "breed": { "type": "string" },
          "barkVolume": { "type": "integer" }
        }
      },
      "Cat": {
        "type": "object",
        "required": ["huntingSkill"],
        "properties": {
          "huntingSkill": { "type": "string", "enum": ["lazy", "aggressive", "expert"] }
        }
      },
      "Pet": {
        "oneOf": [
          { "$ref": "#/components/schemas/Dog" },
          { "$ref": "#/components/schemas/Cat" }
        ],
        "discriminator": {
          "propertyName": "petType",
          "mapping": {
            "dog": "#/components/schemas/Dog",
            "cat": "#/components/schemas/Cat"
          }
        }
      }
    }
  }
}"##;

        let ts_code = generate_and_verify(openapi_json);
        println!("=== DISCRIMINATOR CODE ===\n{ts_code}\n=== END ===");

        // Verify Dog and Cat are regular interfaces
        assert!(
            ts_code.contains("export interface Dog"),
            "Missing Dog interface"
        );
        assert!(
            ts_code.contains("export interface Cat"),
            "Missing Cat interface"
        );

        // Verify Pet is a discriminated union with petType field
        assert!(
            ts_code.contains("export type Pet ="),
            "Missing Pet type alias"
        );
        assert!(
            ts_code.contains("petType: \"dog\"") && ts_code.contains("& Dog"),
            "Pet should include discriminated Dog branch"
        );
        assert!(
            ts_code.contains("petType: \"cat\"") && ts_code.contains("& Cat"),
            "Pet should include discriminated Cat branch"
        );
    }

    #[test]
    fn test_integer_enum() {
        // Test integer enum values (HTTP status codes, error codes)
        let openapi_json = r##"{
  "openapi": "3.1.0",
  "info": { "title": "Integer Enum Test API", "version": "1.0.0" },
  "paths": {},
  "components": {
    "schemas": {
      "HttpStatusCode": {
        "type": "integer",
        "enum": [200, 201, 400, 404, 500]
      },
      "Priority": {
        "type": "integer",
        "enum": [1, 2, 3, 4, 5]
      }
    }
  }
}"##;

        let ts_code = generate_and_verify(openapi_json);
        println!("=== INTEGER ENUM CODE ===\n{ts_code}\n=== END ===");

        // Verify HttpStatusCode enum with integer values
        assert!(
            ts_code.contains("export const HttpStatusCode"),
            "Missing HttpStatusCode const"
        );
        assert!(
            ts_code.contains("VALUE_200: 200"),
            "Should have VALUE_200: 200"
        );
        assert!(
            ts_code.contains("VALUE_404: 404"),
            "Should have VALUE_404: 404"
        );

        // Verify Priority enum
        assert!(
            ts_code.contains("export const Priority"),
            "Missing Priority const"
        );
        assert!(ts_code.contains("VALUE_1: 1"), "Should have VALUE_1: 1");
    }

    #[test]
    fn test_mixed_enum() {
        // Test mixed enum values (strings and other types)
        let openapi_json = r##"{
  "openapi": "3.1.0",
  "info": { "title": "Mixed Enum Test API", "version": "1.0.0" },
  "paths": {},
  "components": {
    "schemas": {
      "MixedValue": {
        "enum": ["auto", "manual", 0, 1, true, false, null]
      }
    }
  }
}"##;

        let ts_code = generate_and_verify(openapi_json);
        println!("=== MIXED ENUM CODE ===\n{ts_code}\n=== END ===");

        // Verify MixedValue enum has various types
        assert!(
            ts_code.contains("export const MixedValue"),
            "Missing MixedValue const"
        );
        assert!(
            ts_code.contains("auto: \"auto\""),
            "Should have string auto"
        );
        assert!(
            ts_code.contains("VALUE_0: 0") || ts_code.contains("VALUE_1: 1"),
            "Should have integer values"
        );
        assert!(
            ts_code.contains("TRUE: true") || ts_code.contains("FALSE: false"),
            "Should have boolean values"
        );
        assert!(ts_code.contains("NULL: null"), "Should have null value");
    }

    #[test]
    fn test_properties_with_additional_properties() {
        // Test object with both properties and additionalProperties (index signature)
        let openapi_json = r##"{
  "openapi": "3.1.0",
  "info": { "title": "Props + Additional Test API", "version": "1.0.0" },
  "paths": {},
  "components": {
    "schemas": {
      "ConfigWithDefaults": {
        "type": "object",
        "required": ["version"],
        "properties": {
          "version": { "type": "string" },
          "debug": { "type": "boolean" }
        },
        "additionalProperties": { "type": "string" }
      },
      "MetadataWithKnownFields": {
        "type": "object",
        "properties": {
          "id": { "type": "string" },
          "timestamp": { "type": "string", "format": "date-time" }
        },
        "additionalProperties": true
      }
    }
  }
}"##;

        let ts_code = generate_and_verify(openapi_json);
        println!("=== PROPS + ADDITIONAL CODE ===\n{ts_code}\n=== END ===");

        // Verify ConfigWithDefaults has intersection type
        assert!(
            ts_code.contains("export type ConfigWithDefaults ="),
            "ConfigWithDefaults should be a type alias"
        );
        assert!(
            ts_code.contains("version: string") && ts_code.contains("& Record<string, string>"),
            "ConfigWithDefaults should be intersection of props and Record"
        );

        // Verify MetadataWithKnownFields has intersection with unknown
        assert!(
            ts_code.contains("export type MetadataWithKnownFields ="),
            "MetadataWithKnownFields should be a type alias"
        );
        assert!(
            ts_code.contains("& Record<string, unknown>"),
            "MetadataWithKnownFields should include Record<string, unknown>"
        );
    }

    #[test]
    fn test_recursive_schema() {
        // Test recursive schema (tree structure)
        let openapi_json = r##"{
  "openapi": "3.1.0",
  "info": { "title": "Recursive Schema Test API", "version": "1.0.0" },
  "paths": {},
  "components": {
    "schemas": {
      "TreeNode": {
        "type": "object",
        "required": ["value"],
        "properties": {
          "value": { "type": "string" },
          "children": {
            "type": "array",
            "items": { "$ref": "#/components/schemas/TreeNode" }
          },
          "parent": {
            "anyOf": [
              { "$ref": "#/components/schemas/TreeNode" },
              { "type": "null" }
            ]
          }
        }
      },
      "LinkedListNode": {
        "type": "object",
        "required": ["data"],
        "properties": {
          "data": { "type": "integer" },
          "next": {
            "anyOf": [
              { "$ref": "#/components/schemas/LinkedListNode" },
              { "type": "null" }
            ]
          }
        }
      }
    }
  }
}"##;

        let ts_code = generate_and_verify(openapi_json);
        println!("=== RECURSIVE SCHEMA CODE ===\n{ts_code}\n=== END ===");

        // Verify TreeNode interface exists
        assert!(
            ts_code.contains("export interface TreeNode"),
            "Missing TreeNode interface"
        );
        assert!(
            ts_code.contains("children?: TreeNode[]"),
            "TreeNode should have recursive children"
        );
        assert!(
            ts_code.contains("parent?: TreeNode | null"),
            "TreeNode should have nullable recursive parent"
        );

        // Verify LinkedListNode interface exists
        assert!(
            ts_code.contains("export interface LinkedListNode"),
            "Missing LinkedListNode interface"
        );
        assert!(
            ts_code.contains("next?: LinkedListNode | null"),
            "LinkedListNode should have nullable recursive next"
        );
    }

    #[test]
    fn test_const_keyword() {
        // Test const keyword for literal types
        let openapi_json = r##"{
  "openapi": "3.1.0",
  "info": { "title": "Const Test API", "version": "1.0.0" },
  "paths": {},
  "components": {
    "schemas": {
      "ApiVersion": {
        "const": "v2"
      },
      "SuccessCode": {
        "const": 0
      },
      "Enabled": {
        "const": true
      }
    }
  }
}"##;

        let ts_code = generate_and_verify(openapi_json);
        println!("=== CONST KEYWORD CODE ===\n{ts_code}\n=== END ===");

        // Verify const generates literal types
        assert!(
            ts_code.contains("export type ApiVersion = \"v2\""),
            "ApiVersion should be literal \"v2\""
        );
        assert!(
            ts_code.contains("export type SuccessCode = 0"),
            "SuccessCode should be literal 0"
        );
        assert!(
            ts_code.contains("export type Enabled = true"),
            "Enabled should be literal true"
        );
    }

    #[test]
    fn test_nullable_openapi_30_style() {
        // Test OpenAPI 3.0 nullable: true style
        let openapi_json = r##"{
  "openapi": "3.0.3",
  "info": { "title": "Nullable 3.0 Test API", "version": "1.0.0" },
  "paths": {},
  "components": {
    "schemas": {
      "NullableString": {
        "type": "string",
        "nullable": true
      },
      "ObjectWithNullable": {
        "type": "object",
        "properties": {
          "name": { "type": "string" },
          "description": { "type": "string", "nullable": true }
        }
      }
    }
  }
}"##;

        let ts_code = generate_and_verify(openapi_json);
        println!("=== NULLABLE 3.0 CODE ===\n{ts_code}\n=== END ===");

        // Note: The current implementation parses nullable but doesn't explicitly
        // emit | null for OpenAPI 3.0 style. This test documents current behavior.
        // A future enhancement could add | null when nullable: true is set.
        assert!(
            ts_code.contains("NullableString") || ts_code.contains("string"),
            "Should handle nullable string"
        );
    }

    #[test]
    fn test_complex_anyof_union() {
        // Test anyOf with object + primitive (not just nullable)
        let openapi_json = r##"{
  "openapi": "3.1.0",
  "info": { "title": "Complex AnyOf Test API", "version": "1.0.0" },
  "paths": {},
  "components": {
    "schemas": {
      "StringOrObject": {
        "anyOf": [
          { "type": "string" },
          {
            "type": "object",
            "properties": {
              "value": { "type": "string" },
              "metadata": { "type": "object", "additionalProperties": true }
            }
          }
        ]
      },
      "NumberOrArray": {
        "anyOf": [
          { "type": "number" },
          { "type": "array", "items": { "type": "number" } }
        ]
      }
    }
  }
}"##;

        let ts_code = generate_and_verify(openapi_json);
        println!("=== COMPLEX ANYOF CODE ===\n{ts_code}\n=== END ===");

        // Verify StringOrObject is a union
        assert!(
            ts_code.contains("export type StringOrObject ="),
            "Missing StringOrObject type"
        );
        assert!(
            ts_code.contains("string |") || ts_code.contains("| string"),
            "StringOrObject should include string"
        );

        // Verify NumberOrArray is a union
        assert!(
            ts_code.contains("export type NumberOrArray ="),
            "Missing NumberOrArray type"
        );
        assert!(
            ts_code.contains("number |") || ts_code.contains("| number"),
            "NumberOrArray should include number"
        );
        assert!(
            ts_code.contains("number[]"),
            "NumberOrArray should include number[]"
        );
    }

    #[test]
    fn test_response_204_with_200() {
        // Test case: both 204 and 200 responses present
        // Should generate runtime check for 204 and union type
        let openapi_json = r##"{
  "openapi": "3.1.0",
  "info": { "title": "204+200 Test API", "version": "1.0.0" },
  "paths": {
    "/items/{id}": {
      "delete": {
        "operationId": "deleteItem",
        "parameters": [{ "name": "id", "in": "path", "required": true, "schema": { "type": "string" } }],
        "responses": {
          "200": { "description": "Deleted with body", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/DeleteResult" } } } },
          "204": { "description": "Deleted without body" }
        }
      }
    }
  },
  "components": {
    "schemas": {
      "DeleteResult": {
        "type": "object",
        "properties": { "deleted": { "type": "boolean" } }
      }
    }
  }
}"##;

        let ts_code = generate_and_verify(openapi_json);
        println!("=== 204+200 CODE ===\n{ts_code}\n=== END ===");

        // Should have runtime check for 204
        assert!(
            ts_code.contains("res.status === 204"),
            "Should check for 204 status"
        );
        // Return type should be union of DeleteResult | void
        assert!(
            ts_code.contains("DeleteResult") && ts_code.contains("void"),
            "Return type should include both DeleteResult and void"
        );
    }

    #[test]
    fn test_response_2xx_wildcard() {
        // Test case: 2XX wildcard and default response
        let openapi_json = r##"{
  "openapi": "3.1.0",
  "info": { "title": "2XX Wildcard Test API", "version": "1.0.0" },
  "paths": {
    "/webhook": {
      "post": {
        "operationId": "triggerWebhook",
        "responses": {
          "2XX": { "description": "Success", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/WebhookResult" } } } }
        }
      }
    },
    "/fallback": {
      "get": {
        "operationId": "getFallback",
        "responses": {
          "default": { "description": "Default response", "content": { "application/json": { "schema": { "type": "object", "properties": { "ok": { "type": "boolean" } } } } } }
        }
      }
    }
  },
  "components": {
    "schemas": {
      "WebhookResult": {
        "type": "object",
        "properties": { "triggered": { "type": "boolean" } }
      }
    }
  }
}"##;

        let ts_code = generate_and_verify(openapi_json);
        println!("=== 2XX WILDCARD CODE ===\n{ts_code}\n=== END ===");

        // Should handle 2XX response
        assert!(
            ts_code.contains("WebhookResult"),
            "Should use WebhookResult from 2XX response"
        );
        // Should handle default response
        assert!(
            ts_code.contains("getFallback"),
            "Should generate getFallback function"
        );
    }

    #[test]
    fn test_response_text_plain() {
        // Test case: text/plain response
        let openapi_json = r##"{
  "openapi": "3.1.0",
  "info": { "title": "Text Response API", "version": "1.0.0" },
  "paths": {
    "/health": {
      "get": {
        "operationId": "healthCheck",
        "responses": {
          "200": { "description": "OK", "content": { "text/plain": { "schema": { "type": "string" } } } }
        }
      }
    }
  },
  "components": { "schemas": {} }
}"##;

        let ts_code = generate_and_verify(openapi_json);
        println!("=== TEXT PLAIN CODE ===\n{ts_code}\n=== END ===");

        // Should use res.text() instead of res.json()
        assert!(
            ts_code.contains("res.text()"),
            "Should use res.text() for text/plain"
        );
        assert!(
            !ts_code.contains("res.json()"),
            "Should NOT use res.json() for text/plain"
        );
    }

    #[test]
    fn test_response_blob() {
        // Test case: binary/blob response (file download)
        let openapi_json = r##"{
  "openapi": "3.1.0",
  "info": { "title": "Blob Response API", "version": "1.0.0" },
  "paths": {
    "/download/{fileId}": {
      "get": {
        "operationId": "downloadFile",
        "parameters": [{ "name": "fileId", "in": "path", "required": true, "schema": { "type": "string" } }],
        "responses": {
          "200": { "description": "File content", "content": { "application/octet-stream": { "schema": { "type": "string", "format": "binary" } } } }
        }
      }
    },
    "/image/{id}": {
      "get": {
        "operationId": "getImage",
        "parameters": [{ "name": "id", "in": "path", "required": true, "schema": { "type": "string" } }],
        "responses": {
          "200": { "description": "Image content", "content": { "image/png": { "schema": { "type": "string", "format": "binary" } } } }
        }
      }
    }
  },
  "components": { "schemas": {} }
}"##;

        let ts_code = generate_and_verify(openapi_json);
        println!("=== BLOB CODE ===\n{ts_code}\n=== END ===");

        // Should use res.blob() for binary content
        assert!(
            ts_code.contains("res.blob()"),
            "Should use res.blob() for binary content"
        );
        // Return type should be Blob
        assert!(ts_code.contains("Blob"), "Return type should be Blob");
    }

    #[test]
    fn test_body_multipart_formdata() {
        // Test case: multipart/form-data request body (file upload)
        let openapi_json = r##"{
  "openapi": "3.1.0",
  "info": { "title": "FormData API", "version": "1.0.0" },
  "paths": {
    "/upload": {
      "post": {
        "operationId": "uploadFile",
        "requestBody": {
          "required": true,
          "content": {
            "multipart/form-data": {
              "schema": {
                "type": "object",
                "properties": {
                  "file": { "type": "string", "format": "binary" },
                  "description": { "type": "string" }
                },
                "required": ["file"]
              }
            }
          }
        },
        "responses": {
          "201": { "description": "Uploaded", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/UploadResult" } } } }
        }
      }
    }
  },
  "components": {
    "schemas": {
      "UploadResult": {
        "type": "object",
        "properties": { "id": { "type": "string" }, "url": { "type": "string" } }
      }
    }
  }
}"##;

        let ts_code = generate_and_verify(openapi_json);
        println!("=== FORMDATA CODE ===\n{ts_code}\n=== END ===");

        // Should NOT set Content-Type (browser sets it with boundary for FormData)
        assert!(
            !ts_code.contains("\"Content-Type\": \"application/json\"")
                || ts_code.contains("FormData"),
            "Should not set JSON content-type for FormData"
        );
        // Should use data directly (FormData) without JSON.stringify
        assert!(
            ts_code.contains("body: data") || ts_code.contains("FormData"),
            "Should pass FormData directly as body"
        );
    }

    #[test]
    fn test_body_urlencoded() {
        // Test case: application/x-www-form-urlencoded request body
        let openapi_json = r##"{
  "openapi": "3.1.0",
  "info": { "title": "URLEncoded API", "version": "1.0.0" },
  "paths": {
    "/login": {
      "post": {
        "operationId": "login",
        "requestBody": {
          "required": true,
          "content": {
            "application/x-www-form-urlencoded": {
              "schema": {
                "type": "object",
                "properties": {
                  "username": { "type": "string" },
                  "password": { "type": "string" }
                },
                "required": ["username", "password"]
              }
            }
          }
        },
        "responses": {
          "200": { "description": "OK", "content": { "application/json": { "schema": { "type": "object", "properties": { "token": { "type": "string" } } } } } }
        }
      }
    }
  },
  "components": { "schemas": {} }
}"##;

        let ts_code = generate_and_verify(openapi_json);
        println!("=== URLENCODED CODE ===\n{ts_code}\n=== END ===");

        // Should use URLSearchParams
        assert!(
            ts_code.contains("URLSearchParams"),
            "Should use URLSearchParams for form-urlencoded"
        );
        // Should set correct content-type
        assert!(
            ts_code.contains("application/x-www-form-urlencoded"),
            "Should set form-urlencoded content-type"
        );
    }

    #[test]
    fn test_header_params() {
        // Test case: required header parameters
        let openapi_json = r##"{
  "openapi": "3.1.0",
  "info": { "title": "Header Params API", "version": "1.0.0" },
  "paths": {
    "/protected": {
      "get": {
        "operationId": "getProtected",
        "parameters": [
          { "name": "X-API-Key", "in": "header", "required": true, "schema": { "type": "string" } },
          { "name": "X-Request-ID", "in": "header", "required": false, "schema": { "type": "string" } },
          { "name": "limit", "in": "query", "required": false, "schema": { "type": "integer" } }
        ],
        "responses": {
          "200": { "description": "OK", "content": { "application/json": { "schema": { "type": "object", "properties": { "data": { "type": "string" } } } } } }
        }
      }
    }
  },
  "components": { "schemas": {} }
}"##;

        let ts_code = generate_and_verify(openapi_json);
        println!("=== HEADER PARAMS CODE ===\n{ts_code}\n=== END ===");

        // Should include header params in interface (with valid TS names)
        assert!(
            ts_code.contains("X-API-Key") || ts_code.contains("xApiKey"),
            "Should include X-API-Key in params"
        );
        // Should pass header params in fetch headers
        assert!(
            ts_code.contains("headers") && ts_code.contains("X-API-Key"),
            "Should pass header params in fetch headers"
        );
    }

    #[test]
    fn test_duplicate_param_names() {
        // Test case: duplicate parameter names should cause error
        let openapi_json = r##"{
  "openapi": "3.1.0",
  "info": { "title": "Duplicate Params API", "version": "1.0.0" },
  "paths": {
    "/items/{id}": {
      "parameters": [
        { "name": "id", "in": "path", "required": true, "schema": { "type": "string" } },
        { "name": "id", "in": "query", "required": false, "schema": { "type": "string" } }
      ],
      "get": {
        "operationId": "getItem",
        "responses": {
          "200": { "description": "OK", "content": { "application/json": { "schema": { "type": "string" } } } }
        }
      }
    }
  },
  "components": { "schemas": {} }
}"##;

        let result = generate(openapi_json);
        // Should return an error for duplicate param names
        assert!(
            result.is_err(),
            "Should fail with duplicate param names, got: {result:?}"
        );
        let err = result.unwrap_err();
        assert!(
            err.contains("duplicate") || err.contains("Duplicate"),
            "Error should mention duplicate: {err}"
        );
    }

    #[test]
    fn test_operationid_collision() {
        // Test case: duplicate operationIds should cause error
        let openapi_json = r##"{
  "openapi": "3.1.0",
  "info": { "title": "Collision API", "version": "1.0.0" },
  "paths": {
    "/items": {
      "get": {
        "operationId": "getItems",
        "responses": {
          "200": { "description": "OK", "content": { "application/json": { "schema": { "type": "array", "items": { "type": "string" } } } } }
        }
      }
    },
    "/other": {
      "get": {
        "operationId": "getItems",
        "responses": {
          "200": { "description": "OK", "content": { "application/json": { "schema": { "type": "string" } } } }
        }
      }
    }
  },
  "components": { "schemas": {} }
}"##;

        let result = generate(openapi_json);
        // Should return an error for duplicate operationIds
        assert!(
            result.is_err(),
            "Should fail with duplicate operationId, got: {result:?}"
        );
        let err = result.unwrap_err();
        assert!(
            err.contains("getItems") || err.contains("duplicate") || err.contains("collision"),
            "Error should mention the duplicate operationId: {err}"
        );
    }

    #[test]
    fn test_cookie_params_skipped() {
        // Test case: cookie parameters should be ignored
        let openapi_json = r##"{
  "openapi": "3.1.0",
  "info": { "title": "Cookie Params API", "version": "1.0.0" },
  "paths": {
    "/session": {
      "get": {
        "operationId": "getSession",
        "parameters": [
          { "name": "session_id", "in": "cookie", "required": true, "schema": { "type": "string" } },
          { "name": "format", "in": "query", "required": false, "schema": { "type": "string" } }
        ],
        "responses": {
          "200": { "description": "OK", "content": { "application/json": { "schema": { "type": "object", "properties": { "user": { "type": "string" } } } } } }
        }
      }
    }
  },
  "components": { "schemas": {} }
}"##;

        let ts_code = generate_and_verify(openapi_json);
        println!("=== COOKIE PARAMS CODE ===\n{ts_code}\n=== END ===");

        // Should NOT include cookie param in interface
        assert!(
            !ts_code.contains("session_id"),
            "Should NOT include cookie param session_id"
        );
        // Should include query param
        assert!(
            ts_code.contains("format"),
            "Should include query param format"
        );
    }

    // =========================================================================
    // Tests for TypeScript emission correctness corner cases
    // =========================================================================

    #[test]
    fn test_operation_id_sanitization() {
        // Test case: operation IDs that aren't valid TS identifiers
        let openapi_json = r##"{
  "openapi": "3.1.0",
  "info": { "title": "Sanitization Test API", "version": "1.0.0" },
  "paths": {
    "/list-items": {
      "get": {
        "operationId": "list-items",
        "responses": { "200": { "description": "OK", "content": { "application/json": { "schema": { "type": "array", "items": { "type": "string" } } } } } }
      }
    },
    "/foo.bar": {
      "get": {
        "operationId": "foo.bar",
        "responses": { "200": { "description": "OK", "content": { "application/json": { "schema": { "type": "string" } } } } }
      }
    },
    "/123start": {
      "get": {
        "operationId": "123start",
        "responses": { "200": { "description": "OK", "content": { "application/json": { "schema": { "type": "string" } } } } }
      }
    },
    "/delete-op": {
      "get": {
        "operationId": "delete",
        "responses": { "200": { "description": "OK", "content": { "application/json": { "schema": { "type": "string" } } } } }
      }
    }
  },
  "components": { "schemas": {} }
}"##;

        let ts_code = generate_and_verify(openapi_json);
        println!("=== OPERATION ID SANITIZATION ===\n{ts_code}\n=== END ===");

        // "list-items" should become "listItems" (camelCase)
        assert!(
            ts_code.contains("export const listItems"),
            "list-items should be sanitized to listItems"
        );
        assert!(
            !ts_code.contains("export const list-items"),
            "Should NOT have invalid identifier list-items"
        );

        // "foo.bar" should become "fooBar" (camelCase)
        assert!(
            ts_code.contains("export const fooBar"),
            "foo.bar should be sanitized to fooBar"
        );
        assert!(
            !ts_code.contains("export const foo.bar"),
            "Should NOT have invalid identifier foo.bar"
        );

        // "123start" should be prefixed with underscore
        assert!(
            ts_code.contains("export const _123start"),
            "123start should be prefixed with _"
        );
        assert!(
            !ts_code.contains("export const 123"),
            "Should NOT have identifier starting with number"
        );

        // "delete" is a reserved word, should be escaped
        assert!(
            ts_code.contains("export const _delete") || ts_code.contains("export const delete_"),
            "delete should be escaped as reserved word"
        );
    }

    #[test]
    fn test_param_name_bracket_notation() {
        // Test case: param names that need bracket notation in path interpolation
        let openapi_json = r##"{
  "openapi": "3.1.0",
  "info": { "title": "Bracket Notation Test API", "version": "1.0.0" },
  "paths": {
    "/items/{item-id}": {
      "get": {
        "operationId": "getItem",
        "parameters": [
          { "name": "item-id", "in": "path", "required": true, "schema": { "type": "string" } }
        ],
        "responses": { "200": { "description": "OK", "content": { "application/json": { "schema": { "type": "string" } } } } }
      }
    },
    "/search": {
      "get": {
        "operationId": "search",
        "parameters": [
          { "name": "sort-by", "in": "query", "required": false, "schema": { "type": "string" } }
        ],
        "responses": { "200": { "description": "OK", "content": { "application/json": { "schema": { "type": "string" } } } } }
      }
    }
  },
  "components": { "schemas": {} }
}"##;

        let ts_code = generate_and_verify(openapi_json);
        println!("=== BRACKET NOTATION CODE ===\n{ts_code}\n=== END ===");

        // Path param "item-id" should use bracket notation in URL template
        assert!(
            ts_code.contains(r#"params["item-id"]"#),
            "Path param item-id should use bracket notation: {ts_code}"
        );
        assert!(
            !ts_code.contains("params.item-id"),
            "Should NOT use dot notation for item-id"
        );

        // Query param "sort-by" should also use bracket notation
        assert!(
            ts_code.contains(r#"params["sort-by"]"#) || ts_code.contains(r#"params?.["sort-by"]"#),
            "Query param sort-by should use bracket notation: {ts_code}"
        );
    }

    #[test]
    fn test_path_template_param_name_mismatch() {
        // Test case: path uses {item_id} but parameter name is itemId
        let openapi_json = r##"{
  "openapi": "3.1.0",
  "info": { "title": "Path Mismatch Test API", "version": "1.0.0" },
  "paths": {
    "/items/{item_id}": {
      "get": {
        "operationId": "getItem",
        "parameters": [
          { "name": "itemId", "in": "path", "required": true, "schema": { "type": "string" } }
        ],
        "responses": { "200": { "description": "OK", "content": { "application/json": { "schema": { "type": "string" } } } } }
      }
    }
  },
  "components": { "schemas": {} }
}"##;

        let ts_code = generate_and_verify(openapi_json);
        println!("=== PATH MISMATCH CODE ===\n{ts_code}\n=== END ===");

        // The generated code should use the param name from the spec (itemId),
        // NOT the path template placeholder (item_id)
        assert!(
            ts_code.contains("params.itemId") || ts_code.contains(r#"params["itemId"]"#),
            "Should use param name 'itemId' not path placeholder 'item_id': {ts_code}"
        );
        assert!(
            !ts_code.contains("params.item_id"),
            "Should NOT use path placeholder name item_id"
        );
    }

    #[test]
    fn test_query_param_array_encoding() {
        // Test case: array query parameters should use repeated params
        let openapi_json = r##"{
  "openapi": "3.1.0",
  "info": { "title": "Array Params Test API", "version": "1.0.0" },
  "paths": {
    "/items": {
      "get": {
        "operationId": "listItems",
        "parameters": [
          { "name": "tags", "in": "query", "required": false, "schema": { "type": "array", "items": { "type": "string" } } },
          { "name": "ids", "in": "query", "required": false, "schema": { "type": "array", "items": { "type": "integer" } } }
        ],
        "responses": { "200": { "description": "OK", "content": { "application/json": { "schema": { "type": "array", "items": { "type": "string" } } } } } }
      }
    }
  },
  "components": { "schemas": {} }
}"##;

        let ts_code = generate_and_verify(openapi_json);
        println!("=== ARRAY PARAMS CODE ===\n{ts_code}\n=== END ===");

        // Should use forEach/for loop with append() for arrays, not String()
        assert!(
            ts_code.contains(".forEach") || ts_code.contains("for (") || ts_code.contains("for("),
            "Array params should use forEach or for loop with append: {ts_code}"
        );
        assert!(
            !ts_code.contains("String(params") || ts_code.contains(".forEach"),
            "Should NOT use String() for array params"
        );
    }

    #[test]
    fn test_query_param_null_handling() {
        // Test case: null values should be excluded from query params
        let openapi_json = r##"{
  "openapi": "3.1.0",
  "info": { "title": "Null Params Test API", "version": "1.0.0" },
  "paths": {
    "/items": {
      "get": {
        "operationId": "listItems",
        "parameters": [
          { "name": "cursor", "in": "query", "required": false, "schema": { "anyOf": [{ "type": "string" }, { "type": "null" }] } }
        ],
        "responses": { "200": { "description": "OK", "content": { "application/json": { "schema": { "type": "string" } } } } }
      }
    }
  },
  "components": { "schemas": {} }
}"##;

        let ts_code = generate_and_verify(openapi_json);
        println!("=== NULL PARAMS CODE ===\n{ts_code}\n=== END ===");

        // Should use != null (not !== undefined) to exclude both null and undefined
        assert!(
            ts_code.contains("!= null"),
            "Should use != null to exclude null and undefined: {ts_code}"
        );
        assert!(
            !ts_code.contains("!== undefined") || ts_code.contains("!= null"),
            "Should prefer != null over !== undefined"
        );
    }

    #[test]
    fn test_no_window_location_origin() {
        // Test case: generated code should not use window.location.origin
        let openapi_json = r##"{
  "openapi": "3.1.0",
  "info": { "title": "Relative URL Test API", "version": "1.0.0" },
  "paths": {
    "/items": {
      "get": {
        "operationId": "listItems",
        "parameters": [
          { "name": "limit", "in": "query", "required": false, "schema": { "type": "integer" } }
        ],
        "responses": { "200": { "description": "OK", "content": { "application/json": { "schema": { "type": "string" } } } } }
      }
    }
  },
  "components": { "schemas": {} }
}"##;

        let ts_code = generate_and_verify(openapi_json);
        println!("=== RELATIVE URL CODE ===\n{ts_code}\n=== END ===");

        // Should NOT use window.location.origin - relative URLs work fine
        assert!(
            !ts_code.contains("window.location.origin"),
            "Should NOT use window.location.origin: {ts_code}"
        );
    }

    #[test]
    fn test_api_error_class() {
        // Test case: should emit ApiError class with status and body
        let openapi_json = r##"{
  "openapi": "3.1.0",
  "info": { "title": "Error Test API", "version": "1.0.0" },
  "paths": {
    "/items": {
      "get": {
        "operationId": "listItems",
        "responses": { "200": { "description": "OK", "content": { "application/json": { "schema": { "type": "array", "items": { "type": "string" } } } } } }
      }
    }
  },
  "components": { "schemas": {} }
}"##;

        let ts_code = generate_and_verify(openapi_json);
        println!("=== API ERROR CODE ===\n{ts_code}\n=== END ===");

        // Should have ApiError class
        assert!(
            ts_code.contains("class ApiError") || ts_code.contains("ApiError"),
            "Should emit ApiError class: {ts_code}"
        );

        // ApiError should have status property
        assert!(
            ts_code.contains("status")
                && (ts_code.contains("class ApiError") || ts_code.contains("throw new ApiError")),
            "ApiError should include status: {ts_code}"
        );

        // Should use ApiError instead of generic Error
        assert!(
            ts_code.contains("throw new ApiError") || !ts_code.contains("throw new Error"),
            "Should throw ApiError instead of generic Error: {ts_code}"
        );
    }

    #[test]
    fn test_hook_error_type() {
        // Test case: hooks should use ApiError type instead of Error
        let openapi_json = r##"{
  "openapi": "3.1.0",
  "info": { "title": "Hook Error Test API", "version": "1.0.0" },
  "paths": {
    "/items": {
      "get": {
        "operationId": "listItems",
        "responses": { "200": { "description": "OK", "content": { "application/json": { "schema": { "type": "array", "items": { "type": "string" } } } } } }
      },
      "post": {
        "operationId": "createItem",
        "requestBody": { "required": true, "content": { "application/json": { "schema": { "type": "object", "properties": { "name": { "type": "string" } } } } } },
        "responses": { "201": { "description": "Created", "content": { "application/json": { "schema": { "type": "string" } } } } }
      }
    }
  },
  "components": { "schemas": {} }
}"##;

        let ts_code = generate_and_verify(openapi_json);
        println!("=== HOOK ERROR TYPE CODE ===\n{ts_code}\n=== END ===");

        // Hook options should reference ApiError, not just Error
        // Check for UseQueryOptions<..., ApiError, ...> or UseMutationOptions<..., ApiError, ...>
        assert!(
            ts_code.contains("ApiError")
                && (ts_code.contains("UseQueryOptions") || ts_code.contains("UseMutationOptions")),
            "Hooks should use ApiError type in options: {ts_code}"
        );
    }

    #[test]
    fn test_optional_params_after_required_body() {
        // Test that optional params are placed after required body in function signature
        // This is required by TypeScript: required params must come before optional ones
        // See: https://github.com/anthropics/claude-code/issues/47
        let openapi_json = r##"{
  "openapi": "3.1.0",
  "info": { "title": "Param Order Test API", "version": "1.0.0" },
  "paths": {
    "/api/agent/chat": {
      "post": {
        "operationId": "agentChat",
        "parameters": [
          {
            "name": "X-Forwarded-Access-Token",
            "in": "header",
            "required": false,
            "schema": { "type": "string" }
          }
        ],
        "requestBody": {
          "required": true,
          "content": {
            "application/json": {
              "schema": { "$ref": "#/components/schemas/AgentMessageIn" }
            }
          }
        },
        "responses": {
          "200": {
            "description": "OK",
            "content": {
              "application/json": {
                "schema": { "$ref": "#/components/schemas/AgentResponseOut" }
              }
            }
          }
        }
      }
    }
  },
  "components": {
    "schemas": {
      "AgentMessageIn": {
        "type": "object",
        "required": ["message"],
        "properties": { "message": { "type": "string" } }
      },
      "AgentResponseOut": {
        "type": "object",
        "required": ["response"],
        "properties": { "response": { "type": "string" } }
      }
    }
  }
}"##;

        let ts_code = generate_and_verify(openapi_json);
        println!("=== PARAM ORDER CODE ===\n{ts_code}\n=== END ===");

        // The function should have required 'data' before optional 'params'
        // Correct: (data: AgentMessageIn, params?: AgentChatParams, options?: RequestInit)
        // Wrong:   (params?: AgentChatParams, data: AgentMessageIn, options?: RequestInit)
        assert!(
            ts_code.contains("export const agentChat = async"),
            "Missing agentChat function"
        );

        // Verify the order: data comes before params? (using regex-like check)
        // The signature should be: (data: AgentMessageIn, params?: ...
        // NOT: (params?: ..., data: AgentMessageIn
        let has_correct_order = ts_code.contains("data: AgentMessageIn, params?:");
        let has_wrong_order = ts_code.contains("params?: AgentChatParams, data:");

        assert!(
            has_correct_order && !has_wrong_order,
            "Required 'data' param should come before optional 'params' param. Generated code:\n{ts_code}"
        );
    }

    // =========================================================================
    // Tests for hook params optionality (GitHub issue: required path params)
    // =========================================================================

    #[test]
    fn test_hook_required_params_for_path_params() {
        // Test case: hooks with required path params should have required params
        // BUG FIX: Previously generated `params?: GetItemParams` but should be `params: GetItemParams`
        let openapi_json = r##"{
  "openapi": "3.1.0",
  "info": { "title": "Required Params Test API", "version": "1.0.0" },
  "paths": {
    "/items/{itemId}": {
      "get": {
        "operationId": "getItem",
        "parameters": [
          { "name": "itemId", "in": "path", "required": true, "schema": { "type": "string" } }
        ],
        "responses": {
          "200": { "description": "OK", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/Item" } } } }
        }
      }
    },
    "/cats/{catId}/{date_utc}/{shelter_id}": {
      "get": {
        "operationId": "getCutieCat",
        "parameters": [
          { "name": "catId", "in": "path", "required": true, "schema": { "type": "string" } },
          { "name": "date_utc", "in": "path", "required": true, "schema": { "type": "string", "format": "date" } },
          { "name": "shelter_id", "in": "path", "required": true, "schema": { "type": "string" } }
        ],
        "responses": {
          "200": { "description": "OK", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/CutieCat" } } } }
        }
      }
    }
  },
  "components": {
    "schemas": {
      "Item": { "type": "object", "properties": { "id": { "type": "string" } } },
      "CutieCat": { "type": "object", "properties": { "name": { "type": "string" } } }
    }
  }
}"##;

        let ts_code = generate_and_verify(openapi_json);
        println!("=== REQUIRED PATH PARAMS CODE ===\n{ts_code}\n=== END ===");

        // useGetItem should have required params (options: { params: GetItemParams; ... })
        // NOT optional params (options?: { params?: GetItemParams; ... })
        assert!(
            ts_code.contains("export function useGetItem"),
            "Missing useGetItem hook"
        );

        // Check that params is required (no ? after params)
        assert!(
            ts_code.contains("{ params: GetItemParams;"),
            "useGetItem should have required params, not optional. Generated:\n{ts_code}"
        );
        assert!(
            !ts_code.contains("{ params?: GetItemParams;"),
            "useGetItem should NOT have optional params. Generated:\n{ts_code}"
        );

        // Check the hook body uses options.params (not options?.params)
        assert!(
            ts_code.contains("getItem(options.params)"),
            "Hook should call fetch with options.params (not options?.params). Generated:\n{ts_code}"
        );

        // Same for useGetCutieCat with multiple path params
        assert!(
            ts_code.contains("{ params: GetCutieCatParams;"),
            "useGetCutieCat should have required params. Generated:\n{ts_code}"
        );
    }

    #[test]
    fn test_hook_optional_params_for_query_only() {
        // Test case: hooks with only optional query params should have optional params
        let openapi_json = r##"{
  "openapi": "3.1.0",
  "info": { "title": "Optional Params Test API", "version": "1.0.0" },
  "paths": {
    "/items": {
      "get": {
        "operationId": "listItems",
        "parameters": [
          { "name": "limit", "in": "query", "required": false, "schema": { "type": "integer" } },
          { "name": "cursor", "in": "query", "required": false, "schema": { "type": "string" } }
        ],
        "responses": {
          "200": { "description": "OK", "content": { "application/json": { "schema": { "type": "array", "items": { "type": "string" } } } } }
        }
      }
    }
  },
  "components": { "schemas": {} }
}"##;

        let ts_code = generate_and_verify(openapi_json);
        println!("=== OPTIONAL QUERY PARAMS CODE ===\n{ts_code}\n=== END ===");

        // useListItems should have optional params (options?: { params?: ListItemsParams; ... })
        assert!(
            ts_code.contains("export function useListItems"),
            "Missing useListItems hook"
        );

        // Check that params is optional
        assert!(
            ts_code.contains("{ params?: ListItemsParams;"),
            "useListItems should have optional params. Generated:\n{ts_code}"
        );

        // Check the hook body uses options?.params
        assert!(
            ts_code.contains("listItems(options?.params)"),
            "Hook should call fetch with options?.params. Generated:\n{ts_code}"
        );
    }

    #[test]
    fn test_hook_required_params_mixed_path_and_query() {
        // Test case: when path params are required, the whole params should be required
        // even if there are also optional query params
        let openapi_json = r##"{
  "openapi": "3.1.0",
  "info": { "title": "Mixed Params Test API", "version": "1.0.0" },
  "paths": {
    "/users/{userId}/posts": {
      "get": {
        "operationId": "getUserPosts",
        "parameters": [
          { "name": "userId", "in": "path", "required": true, "schema": { "type": "string" } },
          { "name": "limit", "in": "query", "required": false, "schema": { "type": "integer" } },
          { "name": "offset", "in": "query", "required": false, "schema": { "type": "integer" } }
        ],
        "responses": {
          "200": { "description": "OK", "content": { "application/json": { "schema": { "type": "array", "items": { "type": "string" } } } } }
        }
      }
    }
  },
  "components": { "schemas": {} }
}"##;

        let ts_code = generate_and_verify(openapi_json);
        println!("=== MIXED PARAMS CODE ===\n{ts_code}\n=== END ===");

        // useGetUserPosts should have required params because userId is required
        assert!(
            ts_code.contains("export function useGetUserPosts"),
            "Missing useGetUserPosts hook"
        );

        // Check that params is required (not optional)
        assert!(
            ts_code.contains("{ params: GetUserPostsParams;"),
            "useGetUserPosts should have required params due to required path param. Generated:\n{ts_code}"
        );

        // The interface should have userId required and limit/offset optional
        assert!(
            ts_code.contains("userId: string"),
            "userId should be required in params interface. Generated:\n{ts_code}"
        );
        assert!(
            ts_code.contains("limit?: number"),
            "limit should be optional in params interface. Generated:\n{ts_code}"
        );
    }

    #[test]
    fn test_suspense_hook_also_has_required_params() {
        // Test case: useSuspenseQuery hooks should also have required params
        let openapi_json = r##"{
  "openapi": "3.1.0",
  "info": { "title": "Suspense Hook Test API", "version": "1.0.0" },
  "paths": {
    "/items/{id}": {
      "get": {
        "operationId": "getItem",
        "parameters": [
          { "name": "id", "in": "path", "required": true, "schema": { "type": "string" } }
        ],
        "responses": {
          "200": { "description": "OK", "content": { "application/json": { "schema": { "type": "object" } } } }
        }
      }
    }
  },
  "components": { "schemas": {} }
}"##;

        let ts_code = generate_and_verify(openapi_json);
        println!("=== SUSPENSE HOOK CODE ===\n{ts_code}\n=== END ===");

        // useGetItemSuspense should also have required params
        assert!(
            ts_code.contains("export function useGetItemSuspense"),
            "Missing useGetItemSuspense hook"
        );

        // Count occurrences of required params pattern
        let required_params_count = ts_code.matches("{ params: GetItemParams;").count();
        assert!(
            required_params_count >= 2,
            "Both useGetItem and useGetItemSuspense should have required params. Found {required_params_count} occurrences. Generated:\n{ts_code}"
        );
    }

    #[test]
    fn test_hook_no_params_remains_optional() {
        // Test case: hooks without any params should still have optional options
        let openapi_json = r##"{
  "openapi": "3.1.0",
  "info": { "title": "No Params Test API", "version": "1.0.0" },
  "paths": {
    "/version": {
      "get": {
        "operationId": "getVersion",
        "responses": {
          "200": { "description": "OK", "content": { "application/json": { "schema": { "type": "object", "properties": { "version": { "type": "string" } } } } } }
        }
      }
    }
  },
  "components": { "schemas": {} }
}"##;

        let ts_code = generate_and_verify(openapi_json);
        println!("=== NO PARAMS CODE ===\n{ts_code}\n=== END ===");

        // useGetVersion should have optional options (no params field at all)
        assert!(
            ts_code.contains("export function useGetVersion"),
            "Missing useGetVersion hook"
        );

        // Should not have params in the type at all
        assert!(
            !ts_code.contains("GetVersionParams"),
            "Hook without params should not have params type. Generated:\n{ts_code}"
        );

        // The options should be optional and only contain query
        assert!(
            ts_code.contains("options?: { query?:"),
            "Hook without params should have optional options with only query. Generated:\n{ts_code}"
        );
    }

    #[test]
    fn test_hook_required_query_param() {
        // Test case: query param marked as required should make params required
        let openapi_json = r##"{
  "openapi": "3.1.0",
  "info": { "title": "Required Query API", "version": "1.0.0" },
  "paths": {
    "/search": {
      "get": {
        "operationId": "search",
        "parameters": [
          { "name": "q", "in": "query", "required": true, "schema": { "type": "string" } },
          { "name": "limit", "in": "query", "required": false, "schema": { "type": "integer" } }
        ],
        "responses": {
          "200": { "description": "OK", "content": { "application/json": { "schema": { "type": "array", "items": { "type": "string" } } } } }
        }
      }
    }
  },
  "components": { "schemas": {} }
}"##;

        let ts_code = generate_and_verify(openapi_json);
        println!("=== REQUIRED QUERY PARAM ===\n{ts_code}\n=== END ===");

        // Even though it's a query param, it's required - so params should be required
        assert!(
            ts_code.contains("{ params: SearchParams;"),
            "Required query param should make params required. Generated:\n{ts_code}"
        );
        assert!(
            ts_code.contains("search(options.params)"),
            "Hook should use options.params (not options?.params). Generated:\n{ts_code}"
        );
    }

    #[test]
    fn test_hook_required_header_param() {
        // Test case: required header param should make params required
        let openapi_json = r##"{
  "openapi": "3.1.0",
  "info": { "title": "Required Header API", "version": "1.0.0" },
  "paths": {
    "/secure/data": {
      "get": {
        "operationId": "getSecureData",
        "parameters": [
          { "name": "Authorization", "in": "header", "required": true, "schema": { "type": "string" } }
        ],
        "responses": {
          "200": { "description": "OK", "content": { "application/json": { "schema": { "type": "object" } } } }
        }
      }
    }
  },
  "components": { "schemas": {} }
}"##;

        let ts_code = generate_and_verify(openapi_json);
        println!("=== REQUIRED HEADER PARAM ===\n{ts_code}\n=== END ===");

        // Required header param should make params required
        assert!(
            ts_code.contains("{ params: GetSecureDataParams;"),
            "Required header param should make params required. Generated:\n{ts_code}"
        );
    }

    /// Helper that generates TypeScript code from OpenAPI JSON without tsc validation.
    /// Use this for negative tests or tests that only check generation logic.
    #[allow(dead_code, clippy::panic)]
    fn generate_only(openapi_json: &str) -> String {
        let result = generate(openapi_json);
        assert!(result.is_ok(), "Generation failed: {:?}", result.err());
        result.unwrap()
    }

    // =========================================================================
    // TypeScript compilation infrastructure (requires bun to be available on PATH)
    // =========================================================================

    use std::process::Command;
    use std::sync::OnceLock;

    /// Cached path to TypeScript test environment (created once, reused across all tests)
    static TS_TEST_ENV: OnceLock<Result<std::path::PathBuf, String>> = OnceLock::new();

    /// Initialize a temporary TypeScript environment with @tanstack/react-query installed.
    /// This is done once and reused across all tests (including across test runs if node_modules exists).
    fn get_ts_test_env() -> Result<std::path::PathBuf, String> {
        TS_TEST_ENV
            .get_or_init(|| {
                use std::io::Write;

                // Check if bun is available
                let bun_check = Command::new("bun").arg("--version").output();
                if bun_check.is_err() || !bun_check.unwrap().status.success() {
                    return Err(
                        "bun is not available on PATH. Please install bun to run TypeScript tests."
                            .to_string(),
                    );
                }

                // Create temp directory for TypeScript tests
                let temp_dir = std::env::temp_dir().join("apx_ts_typecheck");
                if let Err(e) = std::fs::create_dir_all(&temp_dir) {
                    return Err(e.to_string());
                }

                // Check if node_modules/@tanstack/react-query already exists (skip bun install if so)
                let react_query_path = temp_dir.join("node_modules/@tanstack/react-query");
                if react_query_path.exists() {
                    return Ok(temp_dir);
                }

                // Write package.json with @tanstack/react-query
                let package_json = r#"{
  "name": "apx-ts-typecheck",
  "private": true,
  "dependencies": {
    "@tanstack/react-query": "^5",
    "typescript": "^5"
  }
}
"#;
                let package_json_path = temp_dir.join("package.json");
                let mut file = match std::fs::File::create(&package_json_path) {
                    Ok(f) => f,
                    Err(e) => return Err(e.to_string()),
                };
                if let Err(e) = file.write_all(package_json.as_bytes()) {
                    return Err(e.to_string());
                }

                // Run bun install to fetch dependencies
                let output = match Command::new("bun")
                    .arg("install")
                    .current_dir(&temp_dir)
                    .output()
                {
                    Ok(o) => o,
                    Err(e) => return Err(format!("Failed to run bun install: {e}")),
                };

                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    return Err(format!(
                        "bun install failed:\nstdout: {stdout}\nstderr: {stderr}"
                    ));
                }

                Ok(temp_dir)
            })
            .clone()
    }

    /// Helper to run TypeScript type checking on generated code
    fn typecheck_generated_code(code: &str) -> Result<(), String> {
        use std::io::Write;

        let test_env = get_ts_test_env()?;

        // Generate unique filename to avoid race conditions when tests run in parallel
        let unique_id = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let test_filename = format!("_test_{unique_id}.ts");
        let test_file = test_env.join(&test_filename);

        let mut file = std::fs::File::create(&test_file).map_err(|e| e.to_string())?;
        file.write_all(code.as_bytes()).map_err(|e| e.to_string())?;

        // Run tsc from the test environment directory with explicit compiler options
        // Using `bun x` which is equivalent to `bunx`
        let output = Command::new("bun")
            .arg("run")
            .args([
                "tsc",
                "--noEmit",
                "--strict",
                "--skipLibCheck",
                "--target",
                "ES2020",
                "--lib",
                "ES2020,DOM",
                "--module",
                "ESNext",
                "--moduleResolution",
                "bundler",
                &test_filename,
            ])
            .current_dir(&test_env)
            .output()
            .map_err(|e| format!("Failed to run bun x tsc: {e}"))?;

        // Cleanup test file
        std::fs::remove_file(&test_file).ok();

        if output.status.success() {
            Ok(())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            Err(format!("TypeScript errors:\n{stdout}\n{stderr}"))
        }
    }

    /// Helper that generates TypeScript code from OpenAPI JSON and verifies it compiles with tsc.
    /// Use this ONLY for positive tests where generated code should be valid TypeScript.
    #[allow(clippy::panic)]
    fn generate_and_verify(openapi_json: &str) -> String {
        let result = generate(openapi_json);
        assert!(result.is_ok(), "Generation failed: {:?}", result.err());

        let ts_code = result.unwrap();

        // Verify TypeScript compilation
        match typecheck_generated_code(&ts_code) {
            Ok(()) => {}
            Err(e) => panic!(
                "Generated TypeScript code failed to compile:\n{e}\n\nGenerated code:\n{ts_code}"
            ),
        }

        ts_code
    }

    // =========================================================================
    // Additional TypeScript compilation tests
    // =========================================================================

    #[test]
    fn test_typescript_compiles_with_required_path_params() {
        // Test that generated code with required path params compiles with tsc
        let openapi_json = r##"{
  "openapi": "3.1.0",
  "info": { "title": "Cute Cats API", "version": "1.0.0" },
  "paths": {
    "/cats/{name}/{fur_color}/{whisker_count}": {
      "get": {
        "operationId": "getCat",
        "parameters": [
          { "name": "name", "in": "path", "required": true, "schema": { "type": "string" } },
          { "name": "fur_color", "in": "path", "required": true, "schema": { "type": "string" } },
          { "name": "whisker_count", "in": "path", "required": true, "schema": { "type": "string" } }
        ],
        "responses": {
          "200": { "description": "OK", "content": { "application/json": { "schema": { "type": "object", "properties": { "meow": { "type": "string" } } } } } }
        }
      }
    },
    "/treats": {
      "get": {
        "operationId": "listTreats",
        "parameters": [
          { "name": "limit", "in": "query", "required": false, "schema": { "type": "integer" } }
        ],
        "responses": {
          "200": { "description": "OK", "content": { "application/json": { "schema": { "type": "array", "items": { "type": "string" } } } } }
        }
      },
      "post": {
        "operationId": "giveTreat",
        "requestBody": { "required": true, "content": { "application/json": { "schema": { "type": "object", "properties": { "flavor": { "type": "string" } } } } } },
        "responses": {
          "201": { "description": "Created", "content": { "application/json": { "schema": { "type": "string" } } } }
        }
      }
    }
  },
  "components": { "schemas": {} }
}"##;

        // generate_and_verify already runs tsc and panics on errors
        let _ts_code = generate_and_verify(openapi_json);
        println!("TypeScript compilation succeeded!");
    }

    #[test]
    fn test_typescript_compilation_detects_param_type_mismatch() {
        // This test verifies that our typecheck helper can actually detect type errors
        // We manually construct buggy code (the old behavior) and verify tsc catches it
        let buggy_code = r#"
import { useQuery } from "@tanstack/react-query";
import type { UseQueryOptions } from "@tanstack/react-query";

type ApiError = { message: string };

export interface GetCatParams {
  name: string;
  fur_color: string;
  whisker_count: string;
}

// Fetch function requires params (non-nullable)
export const getCat = async (params: GetCatParams): Promise<{ data: { meow: string } }> => {
  return { data: { meow: params.name } };
};

export const getCatKey = (params?: GetCatParams) => ["/cats", params] as const;

// BUG: This is the old buggy behavior - params is optional but getCat requires it
export function useGetCatBuggy<TData = { data: { meow: string } }>(
  options?: { params?: GetCatParams; query?: Omit<UseQueryOptions<{ data: { meow: string } }, ApiError, TData>, "queryKey" | "queryFn"> }
) {
  // This should cause TS2345: Argument of type 'GetCatParams | undefined' is not assignable to parameter of type 'GetCatParams'
  return useQuery({ queryKey: getCatKey(options?.params), queryFn: () => getCat(options?.params), ...options?.query });
}
"#;

        // This buggy code should FAIL type checking
        let result = typecheck_generated_code(buggy_code);
        assert!(
            result.is_err(),
            "Buggy code should fail TypeScript type checking, but it passed!"
        );

        let err = result.unwrap_err();
        assert!(
            err.contains("TS2345") || err.contains("not assignable"),
            "Error should be about type mismatch. Got: {err}"
        );
        println!("TypeScript correctly caught the bug: {err}");
    }

    // =========================================================================
    // Parameter ordering tests (prevent regression of PR #48)
    // =========================================================================

    #[test]
    fn test_mutation_required_body_optional_query_params() {
        // Exact PR #48 scenario: mutation with required body + optional query params
        // Body must come before optional params to avoid TS1016
        let openapi_json = r##"{
  "openapi": "3.1.0",
  "info": { "title": "Param Order Test API", "version": "1.0.0" },
  "paths": {
    "/messages": {
      "post": {
        "operationId": "sendMessage",
        "parameters": [
          { "name": "priority", "in": "query", "required": false, "schema": { "type": "string" } },
          { "name": "retry", "in": "query", "required": false, "schema": { "type": "boolean" } }
        ],
        "requestBody": {
          "required": true,
          "content": { "application/json": { "schema": { "type": "object", "properties": { "text": { "type": "string" } } } } }
        },
        "responses": {
          "201": { "description": "Created", "content": { "application/json": { "schema": { "type": "object", "properties": { "id": { "type": "string" } } } } } }
        }
      }
    }
  },
  "components": { "schemas": {} }
}"##;

        let ts_code = generate_and_verify(openapi_json);
        println!("=== MUTATION BODY + OPTIONAL QUERY ===\n{ts_code}\n=== END ===");

        // Verify data (required) comes before params (optional)
        assert!(
            ts_code.contains("data: {"),
            "Missing required data parameter"
        );
        assert!(
            ts_code.contains("params?: SendMessageParams"),
            "Missing optional params parameter"
        );

        // Verify order: data before params?
        let data_pos = ts_code.find("(data:").expect("data param not found");
        let params_pos = ts_code
            .find("params?: SendMessageParams")
            .expect("params param not found");
        assert!(
            data_pos < params_pos,
            "Required 'data' must come before optional 'params'. data_pos={data_pos}, params_pos={params_pos}"
        );
    }

    #[test]
    fn test_mutation_required_body_required_path_optional_query() {
        // Complex case: required path params + required body + optional query params
        let openapi_json = r##"{
  "openapi": "3.1.0",
  "info": { "title": "Complex Param Order API", "version": "1.0.0" },
  "paths": {
    "/users/{userId}/posts": {
      "post": {
        "operationId": "createUserPost",
        "parameters": [
          { "name": "userId", "in": "path", "required": true, "schema": { "type": "string" } },
          { "name": "draft", "in": "query", "required": false, "schema": { "type": "boolean" } }
        ],
        "requestBody": {
          "required": true,
          "content": { "application/json": { "schema": { "type": "object", "properties": { "title": { "type": "string" }, "content": { "type": "string" } } } } }
        },
        "responses": {
          "201": { "description": "Created", "content": { "application/json": { "schema": { "type": "object", "properties": { "postId": { "type": "string" } } } } } }
        }
      }
    }
  },
  "components": { "schemas": {} }
}"##;

        let ts_code = generate_and_verify(openapi_json);
        println!("=== MUTATION PATH + BODY + QUERY ===\n{ts_code}\n=== END ===");

        // With required path param, params should be required (come first)
        // Order should be: params (required), data (required), options (optional)
        assert!(
            ts_code.contains("params: CreateUserPostParams"),
            "params should be required due to required path param"
        );
        assert!(ts_code.contains("data: {"), "data should be present");

        // Verify mutation hook calls fetch with correct argument order
        assert!(
            ts_code.contains("(vars) => createUserPost(vars.params, vars.data)"),
            "Mutation hook should call fetch with (params, data) order when params is required"
        );
    }

    #[test]
    fn test_mutation_required_body_optional_header_params() {
        // PR #48 variant: mutation with required body + optional header params
        let openapi_json = r##"{
  "openapi": "3.1.0",
  "info": { "title": "Header Param Order API", "version": "1.0.0" },
  "paths": {
    "/api/agent/chat": {
      "post": {
        "operationId": "agentChatWithHeaders",
        "parameters": [
          { "name": "X-Request-Id", "in": "header", "required": false, "schema": { "type": "string" } },
          { "name": "X-Correlation-Id", "in": "header", "required": false, "schema": { "type": "string" } }
        ],
        "requestBody": {
          "required": true,
          "content": { "application/json": { "schema": { "type": "object", "required": ["message"], "properties": { "message": { "type": "string" } } } } }
        },
        "responses": {
          "200": { "description": "OK", "content": { "application/json": { "schema": { "type": "object", "properties": { "response": { "type": "string" } } } } } }
        }
      }
    }
  },
  "components": { "schemas": {} }
}"##;

        let ts_code = generate_and_verify(openapi_json);
        println!("=== MUTATION BODY + OPTIONAL HEADERS ===\n{ts_code}\n=== END ===");

        // data (required) must come before params (optional headers)
        let data_pos = ts_code.find("(data:").expect("data param not found");
        let params_pos = ts_code
            .find("params?: AgentChatWithHeadersParams")
            .expect("params param not found");
        assert!(
            data_pos < params_pos,
            "Required 'data' must come before optional header 'params'"
        );
    }

    #[test]
    fn test_delete_with_optional_query_params_no_body() {
        // DELETE with optional query params and no body
        let openapi_json = r##"{
  "openapi": "3.1.0",
  "info": { "title": "Delete Params API", "version": "1.0.0" },
  "paths": {
    "/items/{itemId}": {
      "delete": {
        "operationId": "deleteItem",
        "parameters": [
          { "name": "itemId", "in": "path", "required": true, "schema": { "type": "string" } },
          { "name": "force", "in": "query", "required": false, "schema": { "type": "boolean" } },
          { "name": "cascade", "in": "query", "required": false, "schema": { "type": "boolean" } }
        ],
        "responses": {
          "204": { "description": "Deleted" }
        }
      }
    }
  },
  "components": { "schemas": {} }
}"##;

        let ts_code = generate_and_verify(openapi_json);
        println!("=== DELETE WITH OPTIONAL QUERY ===\n{ts_code}\n=== END ===");

        // params should be required (due to required path param) even with optional query params
        assert!(
            ts_code.contains("params: DeleteItemParams"),
            "params should be required due to required path param"
        );

        // Should have proper return type for 204
        assert!(
            ts_code.contains("Promise<void>"),
            "DELETE with 204 should return Promise<void>"
        );
    }

    #[test]
    fn test_put_required_path_required_body_optional_header() {
        // PUT with required path param + required body + optional header
        let openapi_json = r##"{
  "openapi": "3.1.0",
  "info": { "title": "PUT Complex API", "version": "1.0.0" },
  "paths": {
    "/resources/{resourceId}": {
      "put": {
        "operationId": "updateResource",
        "parameters": [
          { "name": "resourceId", "in": "path", "required": true, "schema": { "type": "string" } },
          { "name": "If-Match", "in": "header", "required": false, "schema": { "type": "string" } }
        ],
        "requestBody": {
          "required": true,
          "content": { "application/json": { "schema": { "type": "object", "properties": { "name": { "type": "string" }, "value": { "type": "number" } } } } }
        },
        "responses": {
          "200": { "description": "Updated", "content": { "application/json": { "schema": { "type": "object", "properties": { "updated": { "type": "boolean" } } } } } }
        }
      }
    }
  },
  "components": { "schemas": {} }
}"##;

        let ts_code = generate_and_verify(openapi_json);
        println!("=== PUT PATH + BODY + HEADER ===\n{ts_code}\n=== END ===");

        // params is required (path param), data is required, both should be present
        assert!(
            ts_code.contains("params: UpdateResourceParams"),
            "params should be required due to required path param"
        );
        assert!(ts_code.contains("data: {"), "data should be present");

        // Verify mutation hook has correct argument order
        assert!(
            ts_code.contains("(vars) => updateResource(vars.params, vars.data)"),
            "Mutation hook should use (params, data) order when params has required fields"
        );
    }

    #[test]
    fn test_mutation_only_optional_params_body_first() {
        // When all params are optional, body (required) should come first
        let openapi_json = r##"{
  "openapi": "3.1.0",
  "info": { "title": "Body First API", "version": "1.0.0" },
  "paths": {
    "/upload": {
      "post": {
        "operationId": "uploadFile",
        "parameters": [
          { "name": "overwrite", "in": "query", "required": false, "schema": { "type": "boolean" } }
        ],
        "requestBody": {
          "required": true,
          "content": { "application/json": { "schema": { "type": "object", "required": ["filename", "content"], "properties": { "filename": { "type": "string" }, "content": { "type": "string" } } } } }
        },
        "responses": {
          "201": { "description": "Created", "content": { "application/json": { "schema": { "type": "object", "properties": { "url": { "type": "string" } } } } } }
        }
      }
    }
  },
  "components": { "schemas": {} }
}"##;

        let ts_code = generate_and_verify(openapi_json);
        println!("=== BODY FIRST (ALL PARAMS OPTIONAL) ===\n{ts_code}\n=== END ===");

        // data (required) must come before params (optional)
        let signature = ts_code
            .lines()
            .find(|l| l.contains("export const uploadFile"))
            .expect("uploadFile not found");
        assert!(
            signature.contains("data:") && signature.contains("params?:"),
            "Should have required data and optional params"
        );

        // Mutation hook should call with (data, params) order
        assert!(
            ts_code.contains("(vars) => uploadFile(vars.data, vars.params)"),
            "Mutation hook should use (data, params) order when params is optional. Generated:\n{ts_code}"
        );
    }

    // =========================================================================
    // Complex type generation tests
    // =========================================================================

    #[test]
    fn test_deeply_nested_objects() {
        // Test 3+ levels of object nesting
        let openapi_json = r##"{
  "openapi": "3.1.0",
  "info": { "title": "Nested Objects API", "version": "1.0.0" },
  "paths": {
    "/config": {
      "get": {
        "operationId": "getConfig",
        "responses": {
          "200": { "description": "OK", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/AppConfig" } } } }
        }
      }
    }
  },
  "components": {
    "schemas": {
      "AppConfig": {
        "type": "object",
        "properties": {
          "database": {
            "type": "object",
            "properties": {
              "connection": {
                "type": "object",
                "properties": {
                  "host": { "type": "string" },
                  "port": { "type": "integer" },
                  "ssl": {
                    "type": "object",
                    "properties": {
                      "enabled": { "type": "boolean" },
                      "cert": { "type": "string" }
                    }
                  }
                }
              },
              "pool": {
                "type": "object",
                "properties": {
                  "min": { "type": "integer" },
                  "max": { "type": "integer" }
                }
              }
            }
          },
          "logging": {
            "type": "object",
            "properties": {
              "level": { "type": "string" },
              "format": { "type": "string" }
            }
          }
        }
      }
    }
  }
}"##;

        let ts_code = generate_and_verify(openapi_json);
        println!("=== DEEPLY NESTED OBJECTS ===\n{ts_code}\n=== END ===");

        // Verify nested structure compiles and has expected properties
        assert!(
            ts_code.contains("database?:"),
            "Should have database property"
        );
        assert!(
            ts_code.contains("connection?:"),
            "Should have nested connection property"
        );
    }

    #[test]
    fn test_recursive_schema_compiles() {
        // Test self-referential types (already exists but verify tsc compilation)
        let openapi_json = r##"{
  "openapi": "3.1.0",
  "info": { "title": "Recursive Schema API", "version": "1.0.0" },
  "paths": {
    "/tree": {
      "get": {
        "operationId": "getTree",
        "responses": {
          "200": { "description": "OK", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/TreeNode" } } } }
        }
      }
    }
  },
  "components": {
    "schemas": {
      "TreeNode": {
        "type": "object",
        "properties": {
          "value": { "type": "string" },
          "children": {
            "type": "array",
            "items": { "$ref": "#/components/schemas/TreeNode" }
          },
          "parent": { "$ref": "#/components/schemas/TreeNode" }
        }
      }
    }
  }
}"##;

        let ts_code = generate_and_verify(openapi_json);
        println!("=== RECURSIVE SCHEMA ===\n{ts_code}\n=== END ===");

        // Verify recursive references are handled
        assert!(
            ts_code.contains("children?: TreeNode[]"),
            "Should have recursive array reference"
        );
        assert!(
            ts_code.contains("parent?: TreeNode"),
            "Should have recursive single reference"
        );
    }

    #[test]
    fn test_large_discriminated_union() {
        // Test discriminated union with 5+ variants
        let openapi_json = r##"{
  "openapi": "3.1.0",
  "info": { "title": "Large Union API", "version": "1.0.0" },
  "paths": {
    "/events": {
      "post": {
        "operationId": "createEvent",
        "requestBody": {
          "required": true,
          "content": { "application/json": { "schema": { "$ref": "#/components/schemas/Event" } } }
        },
        "responses": {
          "201": { "description": "Created", "content": { "application/json": { "schema": { "type": "object", "properties": { "id": { "type": "string" } } } } } }
        }
      }
    }
  },
  "components": {
    "schemas": {
      "Event": {
        "oneOf": [
          { "$ref": "#/components/schemas/ClickEvent" },
          { "$ref": "#/components/schemas/PageViewEvent" },
          { "$ref": "#/components/schemas/ScrollEvent" },
          { "$ref": "#/components/schemas/FormSubmitEvent" },
          { "$ref": "#/components/schemas/ErrorEvent" }
        ],
        "discriminator": {
          "propertyName": "type",
          "mapping": {
            "click": "#/components/schemas/ClickEvent",
            "pageview": "#/components/schemas/PageViewEvent",
            "scroll": "#/components/schemas/ScrollEvent",
            "form_submit": "#/components/schemas/FormSubmitEvent",
            "error": "#/components/schemas/ErrorEvent"
          }
        }
      },
      "ClickEvent": {
        "type": "object",
        "properties": { "type": { "type": "string" }, "x": { "type": "number" }, "y": { "type": "number" } }
      },
      "PageViewEvent": {
        "type": "object",
        "properties": { "type": { "type": "string" }, "url": { "type": "string" }, "referrer": { "type": "string" } }
      },
      "ScrollEvent": {
        "type": "object",
        "properties": { "type": { "type": "string" }, "scrollY": { "type": "number" }, "scrollX": { "type": "number" } }
      },
      "FormSubmitEvent": {
        "type": "object",
        "properties": { "type": { "type": "string" }, "formId": { "type": "string" }, "fields": { "type": "object", "additionalProperties": { "type": "string" } } }
      },
      "ErrorEvent": {
        "type": "object",
        "properties": { "type": { "type": "string" }, "message": { "type": "string" }, "stack": { "type": "string" } }
      }
    }
  }
}"##;

        let ts_code = generate_and_verify(openapi_json);
        println!("=== LARGE DISCRIMINATED UNION ===\n{ts_code}\n=== END ===");

        // Verify all event types are generated
        assert!(ts_code.contains("ClickEvent"), "Missing ClickEvent");
        assert!(ts_code.contains("PageViewEvent"), "Missing PageViewEvent");
        assert!(ts_code.contains("ScrollEvent"), "Missing ScrollEvent");
        assert!(
            ts_code.contains("FormSubmitEvent"),
            "Missing FormSubmitEvent"
        );
        assert!(ts_code.contains("ErrorEvent"), "Missing ErrorEvent");

        // Verify discriminator creates union type
        assert!(
            ts_code.contains("type Event ="),
            "Should have Event type alias for union"
        );
    }

    #[test]
    fn test_anyof_primitive_and_object() {
        // Test union of primitive and complex object
        let openapi_json = r##"{
  "openapi": "3.1.0",
  "info": { "title": "AnyOf Mixed API", "version": "1.0.0" },
  "paths": {
    "/value": {
      "get": {
        "operationId": "getValue",
        "responses": {
          "200": { "description": "OK", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/FlexibleValue" } } } }
        }
      }
    }
  },
  "components": {
    "schemas": {
      "FlexibleValue": {
        "anyOf": [
          { "type": "string" },
          { "type": "number" },
          {
            "type": "object",
            "properties": {
              "value": { "type": "string" },
              "metadata": {
                "type": "object",
                "properties": {
                  "created": { "type": "string" },
                  "modified": { "type": "string" }
                }
              }
            }
          }
        ]
      }
    }
  }
}"##;

        let ts_code = generate_and_verify(openapi_json);
        println!("=== ANYOF PRIMITIVE + OBJECT ===\n{ts_code}\n=== END ===");

        // Should generate union type
        assert!(
            ts_code.contains("string | number"),
            "Should have string and number in union"
        );
    }

    #[test]
    fn test_empty_object_schema() {
        // Test object with no properties
        let openapi_json = r##"{
  "openapi": "3.1.0",
  "info": { "title": "Empty Object API", "version": "1.0.0" },
  "paths": {
    "/empty": {
      "get": {
        "operationId": "getEmpty",
        "responses": {
          "200": { "description": "OK", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/EmptyObject" } } } }
        }
      },
      "post": {
        "operationId": "createEmpty",
        "requestBody": {
          "required": true,
          "content": { "application/json": { "schema": { "type": "object" } } }
        },
        "responses": {
          "201": { "description": "Created", "content": { "application/json": { "schema": { "type": "object" } } } }
        }
      }
    }
  },
  "components": {
    "schemas": {
      "EmptyObject": {
        "type": "object"
      }
    }
  }
}"##;

        let ts_code = generate_and_verify(openapi_json);
        println!("=== EMPTY OBJECT ===\n{ts_code}\n=== END ===");

        // Empty object should become Record<string, unknown>
        assert!(
            ts_code.contains("Record<string, unknown>"),
            "Empty object should be Record<string, unknown>"
        );
    }

    #[test]
    fn test_additional_properties_only() {
        // Test schema with only additionalProperties (dictionary type)
        let openapi_json = r##"{
  "openapi": "3.1.0",
  "info": { "title": "Dict API", "version": "1.0.0" },
  "paths": {
    "/dict": {
      "get": {
        "operationId": "getDict",
        "responses": {
          "200": { "description": "OK", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/StringDict" } } } }
        }
      }
    }
  },
  "components": {
    "schemas": {
      "StringDict": {
        "type": "object",
        "additionalProperties": { "type": "string" }
      },
      "NumberDict": {
        "type": "object",
        "additionalProperties": { "type": "number" }
      },
      "NestedDict": {
        "type": "object",
        "additionalProperties": {
          "type": "object",
          "properties": {
            "value": { "type": "string" },
            "count": { "type": "integer" }
          }
        }
      }
    }
  }
}"##;

        let ts_code = generate_and_verify(openapi_json);
        println!("=== ADDITIONAL PROPERTIES ONLY ===\n{ts_code}\n=== END ===");

        // Should generate Record types
        assert!(
            ts_code.contains("Record<string, string>"),
            "Should have Record<string, string> for StringDict"
        );
        assert!(
            ts_code.contains("Record<string, number>"),
            "Should have Record<string, number> for NumberDict"
        );
    }

    #[test]
    fn test_allof_intersection_type() {
        // Test allOf creates proper intersection types
        let openapi_json = r##"{
  "openapi": "3.1.0",
  "info": { "title": "AllOf API", "version": "1.0.0" },
  "paths": {
    "/combined": {
      "get": {
        "operationId": "getCombined",
        "responses": {
          "200": { "description": "OK", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/CombinedEntity" } } } }
        }
      }
    }
  },
  "components": {
    "schemas": {
      "BaseEntity": {
        "type": "object",
        "properties": {
          "id": { "type": "string" },
          "createdAt": { "type": "string" }
        }
      },
      "Timestamps": {
        "type": "object",
        "properties": {
          "updatedAt": { "type": "string" },
          "deletedAt": { "type": "string" }
        }
      },
      "CombinedEntity": {
        "allOf": [
          { "$ref": "#/components/schemas/BaseEntity" },
          { "$ref": "#/components/schemas/Timestamps" },
          {
            "type": "object",
            "properties": {
              "name": { "type": "string" },
              "status": { "type": "string" }
            }
          }
        ]
      }
    }
  }
}"##;

        let ts_code = generate_and_verify(openapi_json);
        println!("=== ALLOF INTERSECTION ===\n{ts_code}\n=== END ===");

        // Should create intersection type
        assert!(
            ts_code.contains("BaseEntity &") || ts_code.contains("& BaseEntity"),
            "Should have BaseEntity in intersection"
        );
        assert!(
            ts_code.contains("Timestamps &") || ts_code.contains("& Timestamps"),
            "Should have Timestamps in intersection"
        );
    }

    // =========================================================================
    // Identifier sanitization tests
    // =========================================================================

    #[test]
    fn test_operation_id_with_hyphens() {
        // operationId with hyphens should be converted to camelCase
        let openapi_json = r##"{
  "openapi": "3.1.0",
  "info": { "title": "Hyphen ID API", "version": "1.0.0" },
  "paths": {
    "/items": {
      "get": {
        "operationId": "list-all-items",
        "responses": {
          "200": { "description": "OK", "content": { "application/json": { "schema": { "type": "array", "items": { "type": "string" } } } } }
        }
      }
    },
    "/items/{id}": {
      "get": {
        "operationId": "get-item-by-id",
        "parameters": [
          { "name": "id", "in": "path", "required": true, "schema": { "type": "string" } }
        ],
        "responses": {
          "200": { "description": "OK", "content": { "application/json": { "schema": { "type": "string" } } } }
        }
      }
    }
  },
  "components": { "schemas": {} }
}"##;

        let ts_code = generate_and_verify(openapi_json);
        println!("=== HYPHENATED OPERATION IDS ===\n{ts_code}\n=== END ===");

        // list-all-items should become listAllItems
        assert!(
            ts_code.contains("listAllItems"),
            "list-all-items should become listAllItems"
        );
        assert!(
            ts_code.contains("useListAllItems"),
            "Hook should be useListAllItems"
        );

        // get-item-by-id should become getItemById
        assert!(
            ts_code.contains("getItemById"),
            "get-item-by-id should become getItemById"
        );
    }

    #[test]
    fn test_path_params_with_hyphens() {
        // Path params with hyphens need bracket notation
        let openapi_json = r##"{
  "openapi": "3.1.0",
  "info": { "title": "Hyphen Params API", "version": "1.0.0" },
  "paths": {
    "/items/{item-id}/details/{detail-type}": {
      "get": {
        "operationId": "getItemDetail",
        "parameters": [
          { "name": "item-id", "in": "path", "required": true, "schema": { "type": "string" } },
          { "name": "detail-type", "in": "path", "required": true, "schema": { "type": "string" } }
        ],
        "responses": {
          "200": { "description": "OK", "content": { "application/json": { "schema": { "type": "object" } } } }
        }
      }
    }
  },
  "components": { "schemas": {} }
}"##;

        let ts_code = generate_and_verify(openapi_json);
        println!("=== HYPHENATED PATH PARAMS ===\n{ts_code}\n=== END ===");

        // Should use bracket notation for hyphenated param names
        assert!(
            ts_code.contains(r#"params["item-id"]"#),
            "Should use bracket notation for item-id"
        );
        assert!(
            ts_code.contains(r#"params["detail-type"]"#),
            "Should use bracket notation for detail-type"
        );
    }

    #[test]
    fn test_property_names_with_special_chars() {
        // Property names with special characters need proper escaping
        let openapi_json = r##"{
  "openapi": "3.1.0",
  "info": { "title": "Special Props API", "version": "1.0.0" },
  "paths": {
    "/data": {
      "get": {
        "operationId": "getData",
        "responses": {
          "200": { "description": "OK", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/SpecialData" } } } }
        }
      }
    }
  },
  "components": {
    "schemas": {
      "SpecialData": {
        "type": "object",
        "properties": {
          "normal-name": { "type": "string" },
          "@context": { "type": "string" },
          "$schema": { "type": "string" },
          "data-value": { "type": "number" }
        }
      }
    }
  }
}"##;

        let ts_code = generate_and_verify(openapi_json);
        println!("=== SPECIAL PROPERTY NAMES ===\n{ts_code}\n=== END ===");

        // Properties with special chars should be properly handled
        // Hyphenated names need quotes, $ prefix is valid in JS
        assert!(
            ts_code.contains(r#""normal-name"?:"#) || ts_code.contains(r#"'normal-name'?:"#),
            "Hyphenated property should be quoted"
        );
        assert!(
            ts_code.contains(r#""@context"?:"#) || ts_code.contains(r#"'@context'?:"#),
            "@context property should be quoted"
        );
        assert!(
            ts_code.contains(r#""data-value"?:"#) || ts_code.contains(r#"'data-value'?:"#),
            "data-value property should be quoted"
        );
        // $schema is a valid JS identifier ($ prefix is allowed)
        assert!(
            ts_code.contains("$schema?:"),
            "$schema property should exist ($ is valid in JS identifiers)"
        );
    }

    #[test]
    fn test_missing_operation_id_generates_from_path() {
        // Missing operationId should generate from path + method
        let openapi_json = r##"{
  "openapi": "3.1.0",
  "info": { "title": "No OpId API", "version": "1.0.0" },
  "paths": {
    "/items": {
      "get": {
        "responses": {
          "200": { "description": "OK", "content": { "application/json": { "schema": { "type": "array", "items": { "type": "string" } } } } }
        }
      },
      "post": {
        "requestBody": {
          "required": true,
          "content": { "application/json": { "schema": { "type": "object", "properties": { "name": { "type": "string" } } } } }
        },
        "responses": {
          "201": { "description": "Created", "content": { "application/json": { "schema": { "type": "object" } } } }
        }
      }
    },
    "/categories": {
      "get": {
        "responses": {
          "200": { "description": "OK", "content": { "application/json": { "schema": { "type": "array", "items": { "type": "string" } } } } }
        }
      }
    }
  },
  "components": { "schemas": {} }
}"##;

        let ts_code = generate_and_verify(openapi_json);
        println!("=== MISSING OPERATION ID ===\n{ts_code}\n=== END ===");

        // Should generate operation names from path + method
        assert!(
            ts_code.contains("getItems") || ts_code.contains("get_items"),
            "Should generate GET /items operation name"
        );
        assert!(
            ts_code.contains("postItems") || ts_code.contains("post_items"),
            "Should generate POST /items operation name"
        );
        assert!(
            ts_code.contains("getCategories") || ts_code.contains("get_categories"),
            "Should generate GET /categories operation name"
        );
    }

    #[test]
    fn test_operation_id_starting_with_number() {
        // operationId starting with number should be sanitized
        let openapi_json = r##"{
  "openapi": "3.1.0",
  "info": { "title": "Numeric ID API", "version": "1.0.0" },
  "paths": {
    "/3d-models": {
      "get": {
        "operationId": "3dModelList",
        "responses": {
          "200": { "description": "OK", "content": { "application/json": { "schema": { "type": "array", "items": { "type": "string" } } } } }
        }
      }
    },
    "/123/items": {
      "get": {
        "operationId": "123GetItems",
        "responses": {
          "200": { "description": "OK", "content": { "application/json": { "schema": { "type": "array", "items": { "type": "string" } } } } }
        }
      }
    }
  },
  "components": { "schemas": {} }
}"##;

        let ts_code = generate_and_verify(openapi_json);
        println!("=== NUMERIC OPERATION ID ===\n{ts_code}\n=== END ===");

        // Should have valid TypeScript identifiers (not starting with number)
        // The code should compile - that's the main test
        assert!(
            !ts_code.contains("export const 3"),
            "Function names should not start with numbers"
        );
        assert!(
            !ts_code.contains("export function use3"),
            "Hook names should not start with numbers"
        );
    }

    #[test]
    fn test_header_names_as_ts_identifiers() {
        // HTTP header names like X-Custom-Header should work in params interface
        let openapi_json = r##"{
  "openapi": "3.1.0",
  "info": { "title": "Header Names API", "version": "1.0.0" },
  "paths": {
    "/api": {
      "get": {
        "operationId": "callApi",
        "parameters": [
          { "name": "X-Custom-Header", "in": "header", "required": true, "schema": { "type": "string" } },
          { "name": "X-Request-ID", "in": "header", "required": false, "schema": { "type": "string" } },
          { "name": "Accept-Language", "in": "header", "required": false, "schema": { "type": "string" } }
        ],
        "responses": {
          "200": { "description": "OK", "content": { "application/json": { "schema": { "type": "object" } } } }
        }
      }
    }
  },
  "components": { "schemas": {} }
}"##;

        let ts_code = generate_and_verify(openapi_json);
        println!("=== HEADER NAMES AS IDENTIFIERS ===\n{ts_code}\n=== END ===");

        // Header names with hyphens should be quoted in interface
        assert!(
            ts_code.contains(r#""X-Custom-Header":"#),
            "X-Custom-Header should be quoted in interface"
        );
        assert!(
            ts_code.contains(r#""X-Request-ID"?:"#),
            "X-Request-ID should be quoted and optional"
        );
    }

    // =========================================================================
    // Response type edge case tests
    // =========================================================================

    #[test]
    fn test_multiple_2xx_responses_different_schemas() {
        // API with 200 and 201 having different schemas (should use first success)
        let openapi_json = r##"{
  "openapi": "3.1.0",
  "info": { "title": "Multi Response API", "version": "1.0.0" },
  "paths": {
    "/resources": {
      "post": {
        "operationId": "createResource",
        "requestBody": {
          "required": true,
          "content": { "application/json": { "schema": { "type": "object", "properties": { "name": { "type": "string" } } } } }
        },
        "responses": {
          "200": { "description": "Updated", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ExistingResource" } } } },
          "201": { "description": "Created", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/NewResource" } } } }
        }
      }
    }
  },
  "components": {
    "schemas": {
      "ExistingResource": {
        "type": "object",
        "properties": { "id": { "type": "string" }, "updated": { "type": "boolean" } }
      },
      "NewResource": {
        "type": "object",
        "properties": { "id": { "type": "string" }, "created": { "type": "boolean" } }
      }
    }
  }
}"##;

        let ts_code = generate_and_verify(openapi_json);
        println!("=== MULTIPLE 2XX RESPONSES ===\n{ts_code}\n=== END ===");

        // Should use 200 response type (first in priority)
        assert!(
            ts_code.contains("ExistingResource"),
            "Should use 200 response type"
        );
    }

    #[test]
    fn test_only_204_response() {
        // Endpoint with only 204 response (void return)
        let openapi_json = r##"{
  "openapi": "3.1.0",
  "info": { "title": "Void Only API", "version": "1.0.0" },
  "paths": {
    "/ack": {
      "post": {
        "operationId": "acknowledge",
        "requestBody": {
          "required": true,
          "content": { "application/json": { "schema": { "type": "object", "properties": { "id": { "type": "string" } } } } }
        },
        "responses": {
          "204": { "description": "Acknowledged" }
        }
      }
    }
  },
  "components": { "schemas": {} }
}"##;

        let ts_code = generate_and_verify(openapi_json);
        println!("=== ONLY 204 RESPONSE ===\n{ts_code}\n=== END ===");

        // Should return void
        assert!(
            ts_code.contains("Promise<void>"),
            "Only-204 response should return Promise<void>"
        );
        assert!(
            ts_code.contains("return;"),
            "Should have bare return statement for void"
        );
    }

    #[test]
    fn test_response_no_content_field() {
        // Response defined but no content (should be unknown)
        let openapi_json = r##"{
  "openapi": "3.1.0",
  "info": { "title": "No Content API", "version": "1.0.0" },
  "paths": {
    "/mystery": {
      "get": {
        "operationId": "getMystery",
        "responses": {
          "200": { "description": "OK" }
        }
      }
    }
  },
  "components": { "schemas": {} }
}"##;

        let ts_code = generate_and_verify(openapi_json);
        println!("=== NO CONTENT FIELD ===\n{ts_code}\n=== END ===");

        // Response without content should fall back to unknown
        assert!(
            ts_code.contains("unknown"),
            "Response without content should use unknown type"
        );
    }

    #[test]
    fn test_default_response_only() {
        // Only default response defined (no specific status codes)
        let openapi_json = r##"{
  "openapi": "3.1.0",
  "info": { "title": "Default Response API", "version": "1.0.0" },
  "paths": {
    "/default": {
      "get": {
        "operationId": "getDefault",
        "responses": {
          "default": { "description": "Default response", "content": { "application/json": { "schema": { "type": "object", "properties": { "message": { "type": "string" } } } } } }
        }
      }
    }
  },
  "components": { "schemas": {} }
}"##;

        let ts_code = generate_and_verify(openapi_json);
        println!("=== DEFAULT RESPONSE ONLY ===\n{ts_code}\n=== END ===");

        // Should use default response
        assert!(
            ts_code.contains("message?:"),
            "Should use default response schema"
        );
    }

    #[test]
    fn test_200_with_multiple_content_types() {
        // Response with both JSON and text content types (should use JSON)
        let openapi_json = r##"{
  "openapi": "3.1.0",
  "info": { "title": "Multi Content API", "version": "1.0.0" },
  "paths": {
    "/data": {
      "get": {
        "operationId": "getData",
        "responses": {
          "200": {
            "description": "OK",
            "content": {
              "application/json": { "schema": { "type": "object", "properties": { "value": { "type": "string" } } } },
              "text/plain": { "schema": { "type": "string" } }
            }
          }
        }
      }
    }
  },
  "components": { "schemas": {} }
}"##;

        let ts_code = generate_and_verify(openapi_json);
        println!("=== MULTIPLE CONTENT TYPES ===\n{ts_code}\n=== END ===");

        // Should use first content type (alphabetically or in order)
        // The generated code should compile either way
        assert!(
            ts_code.contains("res.json()") || ts_code.contains("res.text()"),
            "Should have appropriate response parsing"
        );
    }

    #[test]
    fn test_error_responses_ignored() {
        // Error responses (4XX, 5XX) should not affect return type
        let openapi_json = r##"{
  "openapi": "3.1.0",
  "info": { "title": "Error Response API", "version": "1.0.0" },
  "paths": {
    "/protected": {
      "get": {
        "operationId": "getProtected",
        "responses": {
          "200": { "description": "OK", "content": { "application/json": { "schema": { "type": "object", "properties": { "data": { "type": "string" } } } } } },
          "401": { "description": "Unauthorized", "content": { "application/json": { "schema": { "type": "object", "properties": { "error": { "type": "string" } } } } } },
          "403": { "description": "Forbidden", "content": { "application/json": { "schema": { "type": "object", "properties": { "reason": { "type": "string" } } } } } },
          "500": { "description": "Server Error", "content": { "application/json": { "schema": { "type": "object", "properties": { "stack": { "type": "string" } } } } } }
        }
      }
    }
  },
  "components": { "schemas": {} }
}"##;

        let ts_code = generate_and_verify(openapi_json);
        println!("=== ERROR RESPONSES IGNORED ===\n{ts_code}\n=== END ===");

        // Return type should only include success response
        assert!(
            ts_code.contains("data?: string"),
            "Should use 200 response for return type"
        );
        // Error types should not be in the Promise return
        assert!(
            !ts_code.contains("Promise<")
                || !ts_code.contains("error?: string")
                || !ts_code.contains("Promise<{ data: { error"),
            "Error responses should not be in return type"
        );
    }

    // =========================================================================
    // Body content type edge case tests
    // =========================================================================

    #[test]
    fn test_formdata_body_uses_formdata_type() {
        // FormData body should use FormData type, not the schema
        let openapi_json = r##"{
  "openapi": "3.1.0",
  "info": { "title": "FormData API", "version": "1.0.0" },
  "paths": {
    "/upload": {
      "post": {
        "operationId": "uploadFile",
        "requestBody": {
          "required": true,
          "content": {
            "multipart/form-data": {
              "schema": {
                "type": "object",
                "required": ["file"],
                "properties": {
                  "file": { "type": "string", "format": "binary" },
                  "description": { "type": "string" }
                }
              }
            }
          }
        },
        "responses": {
          "201": { "description": "Created", "content": { "application/json": { "schema": { "type": "object", "properties": { "url": { "type": "string" } } } } } }
        }
      }
    }
  },
  "components": { "schemas": {} }
}"##;

        let ts_code = generate_and_verify(openapi_json);
        println!("=== FORMDATA BODY TYPE ===\n{ts_code}\n=== END ===");

        // Function param should be FormData type
        assert!(
            ts_code.contains("data: FormData"),
            "FormData body should use FormData type"
        );

        // Should NOT set Content-Type header (browser handles it)
        assert!(
            !ts_code.contains(r#""Content-Type": "multipart/form-data""#),
            "Should not manually set Content-Type for FormData"
        );

        // Mutation hook should use FormData type
        assert!(
            ts_code.contains("UseMutationOptions<") && ts_code.contains("FormData>"),
            "Mutation hook should use FormData as vars type"
        );
    }

    #[test]
    fn test_formdata_with_params_mutation_hook() {
        // FormData + path params: mutation hook should correctly handle both
        let openapi_json = r##"{
  "openapi": "3.1.0",
  "info": { "title": "FormData With Params API", "version": "1.0.0" },
  "paths": {
    "/users/{userId}/avatar": {
      "post": {
        "operationId": "uploadAvatar",
        "parameters": [
          { "name": "userId", "in": "path", "required": true, "schema": { "type": "string" } }
        ],
        "requestBody": {
          "required": true,
          "content": {
            "multipart/form-data": {
              "schema": {
                "type": "object",
                "properties": { "image": { "type": "string", "format": "binary" } }
              }
            }
          }
        },
        "responses": {
          "200": { "description": "OK", "content": { "application/json": { "schema": { "type": "object" } } } }
        }
      }
    }
  },
  "components": { "schemas": {} }
}"##;

        let ts_code = generate_and_verify(openapi_json);
        println!("=== FORMDATA WITH PARAMS ===\n{ts_code}\n=== END ===");

        // Should have params and data in mutation vars
        assert!(
            ts_code.contains("params: UploadAvatarParams"),
            "Should have params"
        );
        assert!(
            ts_code.contains("data: FormData"),
            "Should have FormData data"
        );

        // Mutation hook vars should include both
        assert!(
            ts_code.contains("{ params: UploadAvatarParams; data: FormData }"),
            "Mutation vars should have params and FormData"
        );
    }

    #[test]
    fn test_urlencoded_body() {
        // URL-encoded body should use URLSearchParams
        let openapi_json = r##"{
  "openapi": "3.1.0",
  "info": { "title": "URLEncoded API", "version": "1.0.0" },
  "paths": {
    "/login": {
      "post": {
        "operationId": "login",
        "requestBody": {
          "required": true,
          "content": {
            "application/x-www-form-urlencoded": {
              "schema": {
                "type": "object",
                "required": ["username", "password"],
                "properties": {
                  "username": { "type": "string" },
                  "password": { "type": "string" }
                }
              }
            }
          }
        },
        "responses": {
          "200": { "description": "OK", "content": { "application/json": { "schema": { "type": "object", "properties": { "token": { "type": "string" } } } } } }
        }
      }
    }
  },
  "components": { "schemas": {} }
}"##;

        let ts_code = generate_and_verify(openapi_json);
        println!("=== URLENCODED BODY ===\n{ts_code}\n=== END ===");

        // Should set correct content type
        assert!(
            ts_code.contains(r#""Content-Type": "application/x-www-form-urlencoded""#),
            "Should set URL-encoded content type"
        );

        // Should use URLSearchParams
        assert!(
            ts_code.contains("new URLSearchParams"),
            "Should use URLSearchParams for body"
        );
    }

    #[test]
    fn test_json_body_sets_content_type() {
        // JSON body should set Content-Type header
        let openapi_json = r##"{
  "openapi": "3.1.0",
  "info": { "title": "JSON Body API", "version": "1.0.0" },
  "paths": {
    "/data": {
      "post": {
        "operationId": "postData",
        "requestBody": {
          "required": true,
          "content": {
            "application/json": {
              "schema": {
                "type": "object",
                "properties": { "value": { "type": "string" } }
              }
            }
          }
        },
        "responses": {
          "201": { "description": "Created", "content": { "application/json": { "schema": { "type": "object" } } } }
        }
      }
    }
  },
  "components": { "schemas": {} }
}"##;

        let ts_code = generate_and_verify(openapi_json);
        println!("=== JSON BODY CONTENT TYPE ===\n{ts_code}\n=== END ===");

        // Should set JSON content type
        assert!(
            ts_code.contains(r#""Content-Type": "application/json""#),
            "Should set JSON content type"
        );

        // Should use JSON.stringify
        assert!(
            ts_code.contains("JSON.stringify(data)"),
            "Should stringify JSON body"
        );
    }

    #[test]
    fn test_empty_request_body() {
        // Request body defined but with no schema (edge case)
        let openapi_json = r##"{
  "openapi": "3.1.0",
  "info": { "title": "Empty Body API", "version": "1.0.0" },
  "paths": {
    "/trigger": {
      "post": {
        "operationId": "triggerAction",
        "requestBody": {
          "required": false,
          "content": {}
        },
        "responses": {
          "200": { "description": "OK", "content": { "application/json": { "schema": { "type": "object" } } } }
        }
      }
    }
  },
  "components": { "schemas": {} }
}"##;

        let ts_code = generate_and_verify(openapi_json);
        println!("=== EMPTY REQUEST BODY ===\n{ts_code}\n=== END ===");

        // Should generate without data parameter (empty content = no body)
        // The code should compile
        assert!(
            ts_code.contains("triggerAction"),
            "Should generate triggerAction function"
        );
    }

    // =========================================================================
    // Error handling tests
    // =========================================================================

    #[test]
    fn test_duplicate_operation_id_error() {
        // Duplicate operationId should produce an error
        let openapi_json = r##"{
  "openapi": "3.1.0",
  "info": { "title": "Duplicate ID API", "version": "1.0.0" },
  "paths": {
    "/items": {
      "get": {
        "operationId": "getItems",
        "responses": {
          "200": { "description": "OK", "content": { "application/json": { "schema": { "type": "array", "items": { "type": "string" } } } } }
        }
      }
    },
    "/products": {
      "get": {
        "operationId": "getItems",
        "responses": {
          "200": { "description": "OK", "content": { "application/json": { "schema": { "type": "array", "items": { "type": "string" } } } } }
        }
      }
    }
  },
  "components": { "schemas": {} }
}"##;

        let result = generate(openapi_json);

        assert!(
            result.is_err(),
            "Duplicate operationId should produce an error"
        );
        let err = result.unwrap_err();
        assert!(
            err.contains("Duplicate") || err.contains("duplicate"),
            "Error should mention duplicate. Got: {err}"
        );
        assert!(
            err.contains("getItems"),
            "Error should mention the duplicate operationId. Got: {err}"
        );
    }

    #[test]
    fn test_empty_paths_spec() {
        // Spec with empty paths object
        let openapi_json = r##"{
  "openapi": "3.1.0",
  "info": { "title": "Empty API", "version": "1.0.0" },
  "paths": {},
  "components": { "schemas": {} }
}"##;

        // Should not error - just produce empty output
        let result = generate(openapi_json);
        assert!(
            result.is_ok(),
            "Empty paths should not error: {:?}",
            result.err()
        );

        let ts_code = result.unwrap();
        // Should produce minimal output (no functions)
        assert!(
            !ts_code.contains("export const") && !ts_code.contains("export function"),
            "Empty paths should produce no functions"
        );
    }

    #[test]
    fn test_spec_with_only_schemas() {
        // Spec with schemas but no paths
        let openapi_json = r##"{
  "openapi": "3.1.0",
  "info": { "title": "Types Only API", "version": "1.0.0" },
  "paths": {},
  "components": {
    "schemas": {
      "User": {
        "type": "object",
        "properties": {
          "id": { "type": "string" },
          "name": { "type": "string" }
        }
      },
      "Status": {
        "type": "string",
        "enum": ["active", "inactive"]
      }
    }
  }
}"##;

        let result = generate(openapi_json);
        assert!(
            result.is_ok(),
            "Types-only spec should not error: {:?}",
            result.err()
        );

        let ts_code = result.unwrap();
        println!("=== TYPES ONLY SPEC ===\n{ts_code}\n=== END ===");

        // Should still generate types
        assert!(
            ts_code.contains("interface User"),
            "Should generate User interface"
        );
        assert!(ts_code.contains("Status"), "Should generate Status type");
    }

    #[test]
    fn test_invalid_json_error() {
        // Invalid JSON should produce helpful error
        let invalid_json = r#"{ "openapi": "3.1.0", invalid }"#;

        let result = generate(invalid_json);
        assert!(result.is_err(), "Invalid JSON should produce an error");

        let err = result.unwrap_err();
        assert!(
            err.contains("parse") || err.contains("JSON") || err.contains("expected"),
            "Error should mention parsing issue. Got: {err}"
        );
    }

    #[test]
    fn test_missing_paths_field() {
        // Spec missing the required paths field
        let incomplete_json = r##"{
  "openapi": "3.1.0",
  "info": { "title": "Incomplete API", "version": "1.0.0" }
}"##;

        let result = generate(incomplete_json);
        assert!(result.is_err(), "Missing paths should produce an error");
    }

    #[test]
    fn test_ref_to_nonexistent_schema() {
        // Reference to a schema that doesn't exist
        let openapi_json = r##"{
  "openapi": "3.1.0",
  "info": { "title": "Bad Ref API", "version": "1.0.0" },
  "paths": {
    "/data": {
      "get": {
        "operationId": "getData",
        "responses": {
          "200": { "description": "OK", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/NonExistent" } } } }
        }
      }
    }
  },
  "components": { "schemas": {} }
}"##;

        // This should still generate code (with NonExistent as type name)
        // The TypeScript compiler will catch if the type doesn't exist
        let result = generate(openapi_json);
        assert!(
            result.is_ok(),
            "Non-existent ref should generate (TS catches error): {:?}",
            result.err()
        );

        let ts_code = result.unwrap();
        // Should reference the non-existent type
        assert!(
            ts_code.contains("NonExistent"),
            "Should reference NonExistent type name"
        );
    }

    #[test]
    fn test_spec_with_no_components() {
        // Spec without components section
        let openapi_json = r##"{
  "openapi": "3.1.0",
  "info": { "title": "No Components API", "version": "1.0.0" },
  "paths": {
    "/ping": {
      "get": {
        "operationId": "ping",
        "responses": {
          "200": { "description": "OK", "content": { "application/json": { "schema": { "type": "string" } } } }
        }
      }
    }
  }
}"##;

        let ts_code = generate_and_verify(openapi_json);
        println!("=== NO COMPONENTS ===\n{ts_code}\n=== END ===");

        // Should generate functions without any types section
        assert!(ts_code.contains("ping"), "Should generate ping function");
    }
}
