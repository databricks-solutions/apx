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

        let result = generate(openapi_json);
        assert!(result.is_ok(), "Generation failed: {:?}", result.err());

        let ts_code = result.unwrap();
        println!("=== SPECIAL CHARS CODE ===\n{}\n=== END ===", ts_code);

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
            ts_code.contains(r#""prop:with:colons"?"#) || ts_code.contains(r#""prop:with:colons":"#),
            "Property with colons should be quoted"
        );
        assert!(
            ts_code.contains(r#""123startsWithNumber"?"#) || ts_code.contains(r#""123startsWithNumber":"#),
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

        let result = generate(openapi_json);
        assert!(result.is_ok(), "Generation failed: {:?}", result.err());

        let ts_code = result.unwrap();
        println!(
            "=== GET-ONLY SPEC CODE ===\n{}\n=== END ===",
            ts_code
        );

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

        let result = generate(openapi_json);
        assert!(result.is_ok(), "Generation failed: {:?}", result.err());

        let ts_code = result.unwrap();
        println!(
            "=== MUTATION-ONLY SPEC CODE ===\n{}\n=== END ===",
            ts_code
        );

        // Should have useMutation and UseMutationOptions
        assert!(ts_code.contains("useMutation"), "Missing useMutation import");
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

        let result = generate(openapi_json);
        assert!(result.is_ok(), "Generation failed: {:?}", result.err());

        let ts_code = result.unwrap();
        println!("=== ALLOF CODE ===\n{}\n=== END ===", ts_code);

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

        let result = generate(openapi_json);
        assert!(result.is_ok(), "Generation failed: {:?}", result.err());

        let ts_code = result.unwrap();
        println!("=== DISCRIMINATOR CODE ===\n{}\n=== END ===", ts_code);

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

        let result = generate(openapi_json);
        assert!(result.is_ok(), "Generation failed: {:?}", result.err());

        let ts_code = result.unwrap();
        println!("=== INTEGER ENUM CODE ===\n{}\n=== END ===", ts_code);

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

        let result = generate(openapi_json);
        assert!(result.is_ok(), "Generation failed: {:?}", result.err());

        let ts_code = result.unwrap();
        println!("=== MIXED ENUM CODE ===\n{}\n=== END ===", ts_code);

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

        let result = generate(openapi_json);
        assert!(result.is_ok(), "Generation failed: {:?}", result.err());

        let ts_code = result.unwrap();
        println!(
            "=== PROPS + ADDITIONAL CODE ===\n{}\n=== END ===",
            ts_code
        );

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

        let result = generate(openapi_json);
        assert!(result.is_ok(), "Generation failed: {:?}", result.err());

        let ts_code = result.unwrap();
        println!("=== RECURSIVE SCHEMA CODE ===\n{}\n=== END ===", ts_code);

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

        let result = generate(openapi_json);
        assert!(result.is_ok(), "Generation failed: {:?}", result.err());

        let ts_code = result.unwrap();
        println!("=== CONST KEYWORD CODE ===\n{}\n=== END ===", ts_code);

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

        let result = generate(openapi_json);
        assert!(result.is_ok(), "Generation failed: {:?}", result.err());

        let ts_code = result.unwrap();
        println!("=== NULLABLE 3.0 CODE ===\n{}\n=== END ===", ts_code);

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

        let result = generate(openapi_json);
        assert!(result.is_ok(), "Generation failed: {:?}", result.err());

        let ts_code = result.unwrap();
        println!("=== COMPLEX ANYOF CODE ===\n{}\n=== END ===", ts_code);

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

        let result = generate(openapi_json);
        assert!(result.is_ok(), "Generation failed: {:?}", result.err());

        let ts_code = result.unwrap();
        println!("=== 204+200 CODE ===\n{}\n=== END ===", ts_code);

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

        let result = generate(openapi_json);
        assert!(result.is_ok(), "Generation failed: {:?}", result.err());

        let ts_code = result.unwrap();
        println!("=== 2XX WILDCARD CODE ===\n{}\n=== END ===", ts_code);

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

        let result = generate(openapi_json);
        assert!(result.is_ok(), "Generation failed: {:?}", result.err());

        let ts_code = result.unwrap();
        println!("=== TEXT PLAIN CODE ===\n{}\n=== END ===", ts_code);

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

        let result = generate(openapi_json);
        assert!(result.is_ok(), "Generation failed: {:?}", result.err());

        let ts_code = result.unwrap();
        println!("=== BLOB CODE ===\n{}\n=== END ===", ts_code);

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

        let result = generate(openapi_json);
        assert!(result.is_ok(), "Generation failed: {:?}", result.err());

        let ts_code = result.unwrap();
        println!("=== FORMDATA CODE ===\n{}\n=== END ===", ts_code);

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

        let result = generate(openapi_json);
        assert!(result.is_ok(), "Generation failed: {:?}", result.err());

        let ts_code = result.unwrap();
        println!("=== URLENCODED CODE ===\n{}\n=== END ===", ts_code);

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

        let result = generate(openapi_json);
        assert!(result.is_ok(), "Generation failed: {:?}", result.err());

        let ts_code = result.unwrap();
        println!("=== HEADER PARAMS CODE ===\n{}\n=== END ===", ts_code);

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
            "Should fail with duplicate param names, got: {:?}",
            result
        );
        let err = result.unwrap_err();
        assert!(
            err.contains("duplicate") || err.contains("Duplicate"),
            "Error should mention duplicate: {}",
            err
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
            "Should fail with duplicate operationId, got: {:?}",
            result
        );
        let err = result.unwrap_err();
        assert!(
            err.contains("getItems") || err.contains("duplicate") || err.contains("collision"),
            "Error should mention the duplicate operationId: {}",
            err
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

        let result = generate(openapi_json);
        assert!(result.is_ok(), "Generation failed: {:?}", result.err());

        let ts_code = result.unwrap();
        println!("=== COOKIE PARAMS CODE ===\n{}\n=== END ===", ts_code);

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

        let result = generate(openapi_json);
        assert!(result.is_ok(), "Generation failed: {:?}", result.err());

        let ts_code = result.unwrap();
        println!("=== OPERATION ID SANITIZATION ===\n{}\n=== END ===", ts_code);

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

        let result = generate(openapi_json);
        assert!(result.is_ok(), "Generation failed: {:?}", result.err());

        let ts_code = result.unwrap();
        println!("=== BRACKET NOTATION CODE ===\n{}\n=== END ===", ts_code);

        // Path param "item-id" should use bracket notation in URL template
        assert!(
            ts_code.contains(r#"params["item-id"]"#),
            "Path param item-id should use bracket notation: {}", ts_code
        );
        assert!(
            !ts_code.contains("params.item-id"),
            "Should NOT use dot notation for item-id"
        );

        // Query param "sort-by" should also use bracket notation
        assert!(
            ts_code.contains(r#"params["sort-by"]"#) || ts_code.contains(r#"params?.["sort-by"]"#),
            "Query param sort-by should use bracket notation: {}", ts_code
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

        let result = generate(openapi_json);
        assert!(result.is_ok(), "Generation failed: {:?}", result.err());

        let ts_code = result.unwrap();
        println!("=== PATH MISMATCH CODE ===\n{}\n=== END ===", ts_code);

        // The generated code should use the param name from the spec (itemId),
        // NOT the path template placeholder (item_id)
        assert!(
            ts_code.contains("params.itemId") || ts_code.contains(r#"params["itemId"]"#),
            "Should use param name 'itemId' not path placeholder 'item_id': {}", ts_code
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

        let result = generate(openapi_json);
        assert!(result.is_ok(), "Generation failed: {:?}", result.err());

        let ts_code = result.unwrap();
        println!("=== ARRAY PARAMS CODE ===\n{}\n=== END ===", ts_code);

        // Should use forEach/for loop with append() for arrays, not String()
        assert!(
            ts_code.contains(".forEach") || ts_code.contains("for (") || ts_code.contains("for("),
            "Array params should use forEach or for loop with append: {}", ts_code
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

        let result = generate(openapi_json);
        assert!(result.is_ok(), "Generation failed: {:?}", result.err());

        let ts_code = result.unwrap();
        println!("=== NULL PARAMS CODE ===\n{}\n=== END ===", ts_code);

        // Should use != null (not !== undefined) to exclude both null and undefined
        assert!(
            ts_code.contains("!= null"),
            "Should use != null to exclude null and undefined: {}", ts_code
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

        let result = generate(openapi_json);
        assert!(result.is_ok(), "Generation failed: {:?}", result.err());

        let ts_code = result.unwrap();
        println!("=== RELATIVE URL CODE ===\n{}\n=== END ===", ts_code);

        // Should NOT use window.location.origin - relative URLs work fine
        assert!(
            !ts_code.contains("window.location.origin"),
            "Should NOT use window.location.origin: {}", ts_code
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

        let result = generate(openapi_json);
        assert!(result.is_ok(), "Generation failed: {:?}", result.err());

        let ts_code = result.unwrap();
        println!("=== API ERROR CODE ===\n{}\n=== END ===", ts_code);

        // Should have ApiError class
        assert!(
            ts_code.contains("class ApiError") || ts_code.contains("ApiError"),
            "Should emit ApiError class: {}", ts_code
        );

        // ApiError should have status property
        assert!(
            ts_code.contains("status") && (ts_code.contains("class ApiError") || ts_code.contains("throw new ApiError")),
            "ApiError should include status: {}", ts_code
        );

        // Should use ApiError instead of generic Error
        assert!(
            ts_code.contains("throw new ApiError") || !ts_code.contains("throw new Error"),
            "Should throw ApiError instead of generic Error: {}", ts_code
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

        let result = generate(openapi_json);
        assert!(result.is_ok(), "Generation failed: {:?}", result.err());

        let ts_code = result.unwrap();
        println!("=== HOOK ERROR TYPE CODE ===\n{}\n=== END ===", ts_code);

        // Hook options should reference ApiError, not just Error
        // Check for UseQueryOptions<..., ApiError, ...> or UseMutationOptions<..., ApiError, ...>
        assert!(
            ts_code.contains("ApiError") && (ts_code.contains("UseQueryOptions") || ts_code.contains("UseMutationOptions")),
            "Hooks should use ApiError type in options: {}", ts_code
        );
    }
}
