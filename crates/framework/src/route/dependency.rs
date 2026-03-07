//! Dependency resolution types — execution plans and dependency graph nodes.
//!
//! These types model FastAPI's dependency injection system at the manifest
//! level. They are serializable and contain no Python objects.

use super::manifest::ParamManifest;
use super::primitives::QualName;
use serde::{Deserialize, Serialize};

// ── Dependency steps ────────────────────────────────────────────────────

/// A single step in the compiled dependency execution plan.
///
/// Steps are topologically sorted — can be executed sequentially.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DependencyStep {
    /// Reserved for future native parameter design.
    /// Will be resolved in Rust with no Python call / no GIL.
    ResolveNative {
        /// Qualified name of the native dependency.
        dep_qualname: QualName,
        /// Target kwarg name on the handler.
        target_kwarg: String,
        /// Configuration TBD — will be defined when native parameter design is finalized.
        config: serde_json::Value,
    },

    /// Resolved once per worker (lifecycle dep). Cached value injected per request.
    ResolveLifecycle {
        /// Qualified name of the lifecycle dependency.
        dep_qualname: QualName,
        /// Target kwarg name on the handler.
        target_kwarg: String,
    },

    /// Call a Python function (standard `Depends`).
    /// Inputs are kwargs produced by earlier steps.
    CallPython {
        /// Qualified name of the dependency callable.
        dep_qualname: QualName,
        /// Target kwarg name on the handler.
        target_kwarg: String,
        /// Names of kwargs this step needs from previous steps' outputs.
        inputs: Vec<String>,
        /// True if the function is an async generator (needs cleanup).
        is_generator: bool,
        /// True if the function is async.
        is_async: bool,
        /// FastAPI's `use_cache` dedup key.
        use_cache: bool,
    },

    /// Extract path param from axum's matched params. Rust-native.
    ExtractPath {
        /// Parameter name.
        name: String,
        /// Python type for conversion.
        type_qualname: QualName,
    },

    /// Extract query param from URL. Rust-native.
    ExtractQuery {
        /// Parameter name.
        name: String,
        /// Python type for conversion.
        type_qualname: QualName,
        /// Whether the parameter is required.
        required: bool,
        /// Serialized default value (JSON).
        #[serde(skip_serializing_if = "Option::is_none")]
        default_json: Option<serde_json::Value>,
    },

    /// Extract header value. Rust-native.
    ExtractHeader {
        /// Parameter name.
        name: String,
        /// Wire name (lowercased, hyphenated): `"x-custom-token"`.
        alias: String,
        /// Python type for conversion.
        type_qualname: QualName,
        /// Whether the header is required.
        required: bool,
    },

    /// Extract cookie value. Rust-native.
    ExtractCookie {
        /// Parameter name.
        name: String,
        /// Python type for conversion.
        type_qualname: QualName,
        /// Whether the cookie is required.
        required: bool,
    },

    /// Parse + validate request body via Pydantic `model_validate_json`.
    ValidateBody {
        /// Parameter name.
        name: String,
        /// Pydantic model qualified name.
        model_qualname: QualName,
    },
}

// ── Dependency plan ─────────────────────────────────────────────────────

/// Pre-compiled execution plan for a single route's dependencies.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DependencyPlan {
    /// Topologically sorted steps — execute in order.
    pub steps: Vec<DependencyStep>,
    /// Final kwarg names to pass to the handler (in order).
    pub handler_kwargs: Vec<String>,
    /// Whether any step requires ASGI objects (Request, `solve_dependencies`, etc.).
    pub needs_asgi: bool,
    /// Generator steps that need cleanup after handler returns (indices into `steps`).
    pub generator_cleanup_indices: Vec<usize>,
}

// ── Dependency graph nodes ──────────────────────────────────────────────

/// Scope at which a dependency is instantiated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DepScope {
    /// Created once per worker, shared across all requests (e.g., DB engine).
    Worker,
    /// Created per request (e.g., DB session).
    Request,
}

/// Classification of a dependency for dispatch optimization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DepTier {
    /// Reserved — native parameter design TBD.
    Native,
    /// Resolved per-worker or per-request via apx lifecycle.
    Lifecycle,
    /// Standard FastAPI `Depends()` — called per-request in Python.
    Standard,
}

/// A node in the app-wide dependency graph (manifest-level).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DependencyNode {
    /// Unique identifier: the Python qualname of the dep callable.
    pub qualname: QualName,
    /// How this dep is resolved.
    pub tier: DepTier,
    /// When this dep is instantiated.
    pub scope: DepScope,
    /// Whether the callable is an async generator (yields via context manager).
    pub is_generator: bool,
    /// Whether the callable is async.
    pub is_async: bool,
    /// Qualnames of dependencies this node depends on.
    pub sub_dependencies: Vec<QualName>,
    /// Parameters of the dep function itself (for validation).
    pub params: Vec<ParamManifest>,
}

/// A dependency resolved once per worker lifetime.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LifecycleDepManifest {
    /// Qualified name of the lifecycle dependency callable.
    pub qualname: QualName,
    /// Position in initialization order (topological sort).
    pub init_order: usize,
    /// Position in shutdown order (reverse of init).
    pub shutdown_order: usize,
    /// Scope of the dependency.
    pub scope: DepScope,
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test code uses unwrap/assert for clarity"
)]
mod tests {
    use super::super::manifest::{ParamManifest, ParamSource};
    use super::super::primitives::QualName;
    use super::*;

    #[test]
    fn dependency_step_serde_roundtrip() {
        let step = DependencyStep::CallPython {
            dep_qualname: QualName::new("backend.deps.get_db")
                .unwrap_or_else(|_| QualName::new("x").unwrap_or_else(|_| unreachable!())),
            target_kwarg: "db".to_owned(),
            inputs: vec!["session".to_owned()],
            is_generator: true,
            is_async: true,
            use_cache: false,
        };
        let json = serde_json::to_string(&step).unwrap_or_default();
        let back: DependencyStep =
            serde_json::from_str(&json).unwrap_or_else(|_| DependencyStep::ExtractPath {
                name: String::new(),
                type_qualname: QualName::new("str").unwrap_or_else(|_| unreachable!()),
            });
        assert!(matches!(back, DependencyStep::CallPython { .. }));
    }

    #[test]
    fn dependency_plan_serde_roundtrip() {
        let plan = DependencyPlan {
            steps: vec![DependencyStep::ExtractPath {
                name: "id".to_owned(),
                type_qualname: QualName::new("int").unwrap_or_else(|_| unreachable!()),
            }],
            handler_kwargs: vec!["id".to_owned()],
            needs_asgi: false,
            generator_cleanup_indices: Vec::new(),
        };
        let json = serde_json::to_string(&plan).unwrap_or_default();
        let back: DependencyPlan = serde_json::from_str(&json).unwrap_or_else(|_| DependencyPlan {
            steps: Vec::new(),
            handler_kwargs: Vec::new(),
            needs_asgi: false,
            generator_cleanup_indices: Vec::new(),
        });
        assert_eq!(back.steps.len(), 1);
        assert_eq!(back.handler_kwargs, vec!["id"]);
    }

    #[test]
    fn dependency_step_serde_resolve_native() {
        let step = DependencyStep::ResolveNative {
            dep_qualname: QualName::new("native.dep").unwrap(),
            target_kwarg: "dep".to_owned(),
            config: serde_json::json!({"key": "value"}),
        };
        let json = serde_json::to_string(&step).unwrap();
        let back: DependencyStep = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, DependencyStep::ResolveNative { .. }));
    }

    #[test]
    fn dependency_step_serde_resolve_lifecycle() {
        let step = DependencyStep::ResolveLifecycle {
            dep_qualname: QualName::new("db.engine").unwrap(),
            target_kwarg: "engine".to_owned(),
        };
        let json = serde_json::to_string(&step).unwrap();
        let back: DependencyStep = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, DependencyStep::ResolveLifecycle { .. }));
    }

    #[test]
    fn dependency_step_serde_extract_path() {
        let step = DependencyStep::ExtractPath {
            name: "item_id".to_owned(),
            type_qualname: QualName::new("int").unwrap(),
        };
        let json = serde_json::to_string(&step).unwrap();
        let back: DependencyStep = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, DependencyStep::ExtractPath { .. }));
    }

    #[test]
    fn dependency_step_serde_extract_query_with_default() {
        let step = DependencyStep::ExtractQuery {
            name: "page".to_owned(),
            type_qualname: QualName::new("int").unwrap(),
            required: false,
            default_json: Some(serde_json::json!(1)),
        };
        let json = serde_json::to_string(&step).unwrap();
        let back: DependencyStep = serde_json::from_str(&json).unwrap();
        match back {
            DependencyStep::ExtractQuery {
                default_json,
                required,
                ..
            } => {
                assert!(!required);
                assert!(default_json.is_some());
            }
            _ => panic!("expected ExtractQuery"),
        }
    }

    #[test]
    fn dependency_step_serde_extract_query_no_default() {
        let step = DependencyStep::ExtractQuery {
            name: "q".to_owned(),
            type_qualname: QualName::new("str").unwrap(),
            required: true,
            default_json: None,
        };
        let json = serde_json::to_string(&step).unwrap();
        let back: DependencyStep = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            back,
            DependencyStep::ExtractQuery {
                required: true,
                default_json: None,
                ..
            }
        ));
    }

    #[test]
    fn dependency_step_serde_extract_header() {
        let step = DependencyStep::ExtractHeader {
            name: "x_token".to_owned(),
            alias: "x-token".to_owned(),
            type_qualname: QualName::new("str").unwrap(),
            required: true,
        };
        let json = serde_json::to_string(&step).unwrap();
        let back: DependencyStep = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, DependencyStep::ExtractHeader { .. }));
    }

    #[test]
    fn dependency_step_serde_extract_cookie() {
        let step = DependencyStep::ExtractCookie {
            name: "session_id".to_owned(),
            type_qualname: QualName::new("str").unwrap(),
            required: false,
        };
        let json = serde_json::to_string(&step).unwrap();
        let back: DependencyStep = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, DependencyStep::ExtractCookie { .. }));
    }

    #[test]
    fn dependency_step_serde_validate_body() {
        let step = DependencyStep::ValidateBody {
            name: "item".to_owned(),
            model_qualname: QualName::new("backend.models.Item").unwrap(),
        };
        let json = serde_json::to_string(&step).unwrap();
        let back: DependencyStep = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, DependencyStep::ValidateBody { .. }));
    }

    #[test]
    fn dep_scope_serde_roundtrip() {
        for scope in [DepScope::Worker, DepScope::Request] {
            let json = serde_json::to_string(&scope).unwrap();
            let back: DepScope = serde_json::from_str(&json).unwrap();
            assert_eq!(scope, back);
        }
    }

    #[test]
    fn dep_tier_serde_roundtrip() {
        for tier in [DepTier::Native, DepTier::Lifecycle, DepTier::Standard] {
            let json = serde_json::to_string(&tier).unwrap();
            let back: DepTier = serde_json::from_str(&json).unwrap();
            assert_eq!(tier, back);
        }
    }

    #[test]
    fn dependency_node_serde_roundtrip() {
        let node = DependencyNode {
            qualname: QualName::new("backend.deps.get_db").unwrap(),
            tier: DepTier::Standard,
            scope: DepScope::Request,
            is_generator: true,
            is_async: true,
            sub_dependencies: vec![QualName::new("backend.deps.get_session").unwrap()],
            params: vec![ParamManifest {
                name: "conn_str".to_owned(),
                source: ParamSource::Query,
                type_qualname: QualName::new("str").unwrap(),
                required: true,
                json_schema: None,
                alias: None,
                default_json: None,
            }],
        };
        let json = serde_json::to_string(&node).unwrap();
        let back: DependencyNode = serde_json::from_str(&json).unwrap();
        assert_eq!(back.qualname.as_str(), "backend.deps.get_db");
        assert_eq!(back.sub_dependencies.len(), 1);
        assert_eq!(back.params.len(), 1);
    }

    #[test]
    fn lifecycle_dep_manifest_serde_roundtrip() {
        let dep = LifecycleDepManifest {
            qualname: QualName::new("backend.deps.db_engine").unwrap(),
            init_order: 0,
            shutdown_order: 1,
            scope: DepScope::Worker,
        };
        let json = serde_json::to_string(&dep).unwrap();
        let back: LifecycleDepManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.qualname.as_str(), "backend.deps.db_engine");
        assert_eq!(back.init_order, 0);
        assert_eq!(back.shutdown_order, 1);
    }
}
