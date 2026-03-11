//! Build-time structural validation of manifests.
//!
//! Pure Rust checks — no Python needed. Run after deserializing a manifest
//! to catch structural issues before writing to disk.

use crate::route::{AppManifest, DependencyStep, ParamSource, ValidationCheck};
use std::collections::{HashMap, HashSet};

/// Run all structural validation checks on a manifest.
pub fn validate_all(manifest: &AppManifest) -> Vec<ValidationCheck> {
    vec![
        validate_no_circular_deps(manifest),
        validate_path_params_match(manifest),
        validate_dependency_inputs(manifest),
        validate_no_duplicate_routes(manifest),
    ]
}

/// Verify the dependency graph contains no cycles.
fn validate_no_circular_deps(manifest: &AppManifest) -> ValidationCheck {
    let adj = build_adjacency_list(manifest);
    match topological_sort(&adj) {
        Ok(_) => pass("no_circular_deps"),
        Err(cycle) => fail(
            "no_circular_deps",
            &format!("cycle detected: {}", cycle.join(" -> ")),
        ),
    }
}

/// Verify each route's path template params match its `ParamSource::Path` params.
fn validate_path_params_match(manifest: &AppManifest) -> ValidationCheck {
    let mut mismatches = Vec::new();
    for route in &manifest.routes {
        let template_params = extract_template_params(route.path.as_str());
        let declared_params: HashSet<&str> = route
            .params
            .iter()
            .filter(|p| p.source == ParamSource::Path)
            .map(|p| p.name.as_str())
            .collect();

        for tp in &template_params {
            if !declared_params.contains(tp) {
                mismatches.push(format!(
                    "{} {}: template param '{{{}}}' has no Path param",
                    route.method.as_str(),
                    route.path,
                    tp
                ));
            }
        }
        for dp in &declared_params {
            if !template_params.contains(*dp) {
                mismatches.push(format!(
                    "{} {}: Path param '{}' not in template",
                    route.method.as_str(),
                    route.path,
                    dp
                ));
            }
        }
    }
    if mismatches.is_empty() {
        return pass("path_params_match");
    }
    fail("path_params_match", &mismatches.join("; "))
}

/// Verify each `CallPython` step's inputs reference earlier step outputs or param names.
fn validate_dependency_inputs(manifest: &AppManifest) -> ValidationCheck {
    let mut errors = Vec::new();
    for route in &manifest.routes {
        let Some(plan) = &route.dependency_plan else {
            continue;
        };
        let mut available: HashSet<String> = HashSet::new();
        for step in &plan.steps {
            collect_step_output(&mut available, step);
            check_step_inputs(&available, step, route.path.as_str(), &mut errors);
        }
    }
    if errors.is_empty() {
        return pass("dependency_inputs");
    }
    fail("dependency_inputs", &errors.join("; "))
}

/// Verify no two routes share the same (method, path).
fn validate_no_duplicate_routes(manifest: &AppManifest) -> ValidationCheck {
    let mut seen = HashSet::new();
    let mut dupes = Vec::new();
    for route in &manifest.routes {
        let key = (route.method, route.path.as_str());
        if !seen.insert(key) {
            dupes.push(format!("{} {}", route.method.as_str(), route.path));
        }
    }
    if dupes.is_empty() {
        return pass("no_duplicate_routes");
    }
    fail(
        "no_duplicate_routes",
        &format!("duplicates: {}", dupes.join(", ")),
    )
}

// ── Helpers ──────────────────────────────────────────────────────────────

fn pass(name: &str) -> ValidationCheck {
    ValidationCheck {
        name: name.to_owned(),
        passed: true,
        detail: None,
    }
}

fn fail(name: &str, detail: &str) -> ValidationCheck {
    ValidationCheck {
        name: name.to_owned(),
        passed: false,
        detail: Some(detail.to_owned()),
    }
}

/// Extract `{param}` names from a route path template.
fn extract_template_params(path: &str) -> HashSet<&str> {
    let mut params = HashSet::new();
    let mut rest = path;
    while let Some(open) = rest.find('{') {
        let after_open = &rest[open + 1..];
        let Some(close) = after_open.find('}') else {
            break;
        };
        let name = &after_open[..close];
        // Strip path convertor suffix if present (e.g. "{path:path}")
        let clean = name.split(':').next().unwrap_or(name);
        if !clean.is_empty() {
            params.insert(clean);
        }
        rest = &after_open[close + 1..];
    }
    params
}

/// Build adjacency list from the dependency graph nodes.
fn build_adjacency_list(manifest: &AppManifest) -> HashMap<&str, Vec<&str>> {
    let mut adj: HashMap<&str, Vec<&str>> = HashMap::new();
    for node in &manifest.dependency_graph {
        let deps: Vec<&str> = node.sub_dependencies.iter().map(|q| q.as_str()).collect();
        adj.insert(node.qualname.as_str(), deps);
    }
    adj
}

/// Topological sort via DFS. Returns sorted order or a cycle path.
fn topological_sort<'a>(adj: &HashMap<&'a str, Vec<&'a str>>) -> Result<Vec<&'a str>, Vec<String>> {
    #[derive(Clone, Copy, PartialEq)]
    enum State {
        Unvisited,
        InProgress,
        Done,
    }

    let mut state: HashMap<&str, State> = HashMap::new();
    for &node in adj.keys() {
        state.insert(node, State::Unvisited);
    }

    let mut order = Vec::new();
    let mut path: Vec<&str> = Vec::new();

    for &start in adj.keys() {
        if state.get(start).copied() != Some(State::Unvisited) {
            continue;
        }
        let mut stack: Vec<(&str, bool)> = vec![(start, false)];
        while let Some((node, processed)) = stack.pop() {
            if processed {
                state.insert(node, State::Done);
                path.pop();
                order.push(node);
                continue;
            }
            if state.get(node).copied() == Some(State::Done) {
                continue;
            }
            if state.get(node).copied() == Some(State::InProgress) {
                path.push(node);
                return Err(path.iter().map(|s| (*s).to_owned()).collect());
            }
            state.insert(node, State::InProgress);
            path.push(node);
            stack.push((node, true));
            for &dep in adj.get(node).into_iter().flatten() {
                stack.push((dep, false));
            }
        }
    }
    Ok(order)
}

/// Add the output name produced by a dependency step.
fn collect_step_output(available: &mut HashSet<String>, step: &DependencyStep) {
    match step {
        DependencyStep::CallPython { target_kwarg, .. }
        | DependencyStep::ResolveLifecycle { target_kwarg, .. }
        | DependencyStep::ResolveNative { target_kwarg, .. } => {
            available.insert(target_kwarg.clone());
        }
        DependencyStep::ExtractPath { name, .. }
        | DependencyStep::ExtractQuery { name, .. }
        | DependencyStep::ExtractHeader { name, .. }
        | DependencyStep::ExtractCookie { name, .. }
        | DependencyStep::ValidateBody { name, .. } => {
            available.insert(name.clone());
        }
    }
}

/// Check that a `CallPython` step's inputs reference available names.
fn check_step_inputs(
    available: &HashSet<String>,
    step: &DependencyStep,
    route_path: &str,
    errors: &mut Vec<String>,
) {
    let DependencyStep::CallPython {
        inputs,
        dep_qualname,
        ..
    } = step
    else {
        return;
    };
    for input in inputs {
        if !available.contains(input) {
            errors.push(format!(
                "{route_path}: step '{dep_qualname}' references unknown input '{input}'"
            ));
        }
    }
}

#[cfg(test)]
#[expect(clippy::unwrap_used, reason = "test code uses unwrap for clarity")]
mod tests {
    use super::*;
    use crate::route::*;

    fn make_manifest(routes: Vec<RouteManifest>, graph: Vec<DependencyNode>) -> AppManifest {
        AppManifest {
            meta: None,
            routes,
            dependency_graph: graph,
            lifecycle_deps: Vec::new(),
            openapi_schema: None,
            max_body_limit: BodyLimit::DEFAULT,
            validation_results: Vec::new(),
        }
    }

    fn simple_route(method: HttpMethod, path: &str) -> RouteManifest {
        RouteManifest {
            kind: HandlerKind::RequestResponse,
            method,
            path: RoutePath::new(path).unwrap(),
            handler_qualname: QualName::new("mod.handler").unwrap(),
            params: Vec::new(),
            response_type: ResponseType::RawResponse,
            tags: Vec::new(),
            dependency_plan: None,
            status_code: 200,
            summary: None,
            description: None,
            include_in_schema: true,
            deprecated: false,
            operation_id: None,
            is_async_handler: true,
            dispatch_strategy: DispatchStrategy::default(),
        }
    }

    fn dep_node(name: &str, deps: &[&str]) -> DependencyNode {
        DependencyNode {
            qualname: QualName::new(name).unwrap(),
            tier: DepTier::Standard,
            scope: DepScope::Request,
            is_generator: false,
            is_async: false,
            sub_dependencies: deps.iter().map(|d| QualName::new(*d).unwrap()).collect(),
            params: Vec::new(),
        }
    }

    // ── no_circular_deps ─────────────────────────────────────────────

    #[test]
    fn validate_no_circular_deps_pass() {
        let m = make_manifest(vec![], vec![dep_node("a", &["b"]), dep_node("b", &[])]);
        let check = validate_no_circular_deps(&m);
        assert!(check.passed);
    }

    #[test]
    fn validate_no_circular_deps_cycle() {
        let m = make_manifest(vec![], vec![dep_node("a", &["b"]), dep_node("b", &["a"])]);
        let check = validate_no_circular_deps(&m);
        assert!(!check.passed);
        assert!(check.detail.as_deref().unwrap_or("").contains("cycle"));
    }

    #[test]
    fn validate_no_circular_deps_self_loop() {
        let m = make_manifest(vec![], vec![dep_node("a", &["a"])]);
        let check = validate_no_circular_deps(&m);
        assert!(!check.passed);
    }

    // ── path_params_match ────────────────────────────────────────────

    #[test]
    fn validate_path_params_match_pass() {
        let mut route = simple_route(HttpMethod::Get, "/items/{id}");
        route.params.push(ParamManifest {
            name: "id".to_owned(),
            source: ParamSource::Path,
            type_qualname: QualName::new("int").unwrap(),
            required: true,
            json_schema: None,
            alias: None,
            default_json: None,
        });
        let m = make_manifest(vec![route], vec![]);
        let check = validate_path_params_match(&m);
        assert!(check.passed);
    }

    #[test]
    fn validate_path_params_match_missing_param() {
        let route = simple_route(HttpMethod::Get, "/items/{id}");
        // No Path param declared
        let m = make_manifest(vec![route], vec![]);
        let check = validate_path_params_match(&m);
        assert!(!check.passed);
        assert!(check.detail.as_deref().unwrap_or("").contains("id"));
    }

    #[test]
    fn validate_path_params_match_extra_param() {
        let mut route = simple_route(HttpMethod::Get, "/items");
        route.params.push(ParamManifest {
            name: "id".to_owned(),
            source: ParamSource::Path,
            type_qualname: QualName::new("int").unwrap(),
            required: true,
            json_schema: None,
            alias: None,
            default_json: None,
        });
        let m = make_manifest(vec![route], vec![]);
        let check = validate_path_params_match(&m);
        assert!(!check.passed);
        assert!(
            check
                .detail
                .as_deref()
                .unwrap_or("")
                .contains("not in template")
        );
    }

    // ── dependency_inputs ────────────────────────────────────────────

    #[test]
    fn validate_dependency_inputs_pass() {
        let mut route = simple_route(HttpMethod::Get, "/test");
        route.dependency_plan = Some(DependencyPlan {
            steps: vec![
                DependencyStep::ExtractQuery {
                    name: "page".to_owned(),
                    type_qualname: QualName::new("int").unwrap(),
                    required: true,
                    default_json: None,
                },
                DependencyStep::CallPython {
                    dep_qualname: QualName::new("deps.get_db").unwrap(),
                    target_kwarg: "db".to_owned(),
                    inputs: vec!["page".to_owned()],
                    is_generator: false,
                    is_async: false,
                    use_cache: true,
                },
            ],
            handler_kwargs: vec!["db".to_owned()],
            needs_asgi: false,
            generator_cleanup_indices: vec![],
        });
        let m = make_manifest(vec![route], vec![]);
        let check = validate_dependency_inputs(&m);
        assert!(check.passed);
    }

    #[test]
    fn validate_dependency_inputs_dangling() {
        let mut route = simple_route(HttpMethod::Get, "/test");
        route.dependency_plan = Some(DependencyPlan {
            steps: vec![DependencyStep::CallPython {
                dep_qualname: QualName::new("deps.get_db").unwrap(),
                target_kwarg: "db".to_owned(),
                inputs: vec!["nonexistent".to_owned()],
                is_generator: false,
                is_async: false,
                use_cache: true,
            }],
            handler_kwargs: vec!["db".to_owned()],
            needs_asgi: false,
            generator_cleanup_indices: vec![],
        });
        let m = make_manifest(vec![route], vec![]);
        let check = validate_dependency_inputs(&m);
        assert!(!check.passed);
        assert!(
            check
                .detail
                .as_deref()
                .unwrap_or("")
                .contains("nonexistent")
        );
    }

    // ── no_duplicate_routes ──────────────────────────────────────────

    #[test]
    fn validate_no_duplicate_routes_pass() {
        let m = make_manifest(
            vec![
                simple_route(HttpMethod::Get, "/items"),
                simple_route(HttpMethod::Post, "/items"),
            ],
            vec![],
        );
        let check = validate_no_duplicate_routes(&m);
        assert!(check.passed);
    }

    #[test]
    fn validate_no_duplicate_routes_fail() {
        let m = make_manifest(
            vec![
                simple_route(HttpMethod::Get, "/items"),
                simple_route(HttpMethod::Get, "/items"),
            ],
            vec![],
        );
        let check = validate_no_duplicate_routes(&m);
        assert!(!check.passed);
        assert!(check.detail.as_deref().unwrap_or("").contains("GET /items"));
    }

    // ── validate_all ─────────────────────────────────────────────────

    #[test]
    fn validate_all_clean_manifest() {
        let m = make_manifest(vec![simple_route(HttpMethod::Get, "/health")], vec![]);
        let checks = validate_all(&m);
        assert!(checks.iter().all(|c| c.passed));
    }

    // ── helpers ──────────────────────────────────────────────────────

    #[test]
    fn extract_template_params_basic() {
        let params = extract_template_params("/items/{item_id}/sub/{sub_id}");
        assert!(params.contains("item_id"));
        assert!(params.contains("sub_id"));
        assert_eq!(params.len(), 2);
    }

    #[test]
    #[expect(
        clippy::literal_string_with_formatting_args,
        reason = "testing path convertor syntax"
    )]
    fn extract_template_params_with_convertor() {
        let params = extract_template_params("/files/{path:path}");
        assert!(params.contains("path"));
        assert_eq!(params.len(), 1);
    }
}
