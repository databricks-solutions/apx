"""Manifest extraction for apx build.

Imports a FastAPI app, walks routes, extracts manifest data, and outputs
JSON matching the Rust ``AppManifest`` serde format.

Usage::

    python -m apx._manifest backend.app

Outputs JSON to stdout.
"""

from __future__ import annotations

import importlib
import inspect
import json
import os
import sys
from datetime import datetime, timezone
from typing import Any


# ── Helpers ──────────────────────────────────────────────────────────────


def _qualname(obj: Any) -> str:
    """Return ``module.qualname`` for a callable or type."""
    module = getattr(obj, "__module__", "") or ""
    qn = getattr(obj, "__qualname__", "") or getattr(obj, "__name__", "")
    if module in ("builtins", ""):
        return qn
    return f"{module}.{qn}"


def _type_qualname(type_obj: Any) -> str:
    """Return qualified name for a type annotation."""
    if type_obj is None:
        return "NoneType"
    return _qualname(type_obj)


def _method_from_str(method: str) -> str:
    """Map HTTP method string to Rust enum variant name."""
    return {
        "GET": "Get",
        "POST": "Post",
        "PUT": "Put",
        "DELETE": "Delete",
        "PATCH": "Patch",
    }.get(method.upper(), method)


def _is_streaming_response(type_obj: Any) -> bool:
    """Check if a type is a streaming response."""
    try:
        from starlette.responses import StreamingResponse
        return type_obj is StreamingResponse or (
            isinstance(type_obj, type) and issubclass(type_obj, StreamingResponse)
        )
    except ImportError:
        return False


def _has_model_validate(type_obj: Any) -> bool:
    """Check if a type has Pydantic's model_validate_json."""
    return hasattr(type_obj, "model_validate_json")


def _default_to_json(field_info: Any) -> Any | None:
    """Extract default value as JSON-serializable value, or None."""
    default = getattr(field_info, "default", None)
    if default is None:
        return None
    repr_str = repr(default)
    if "PydanticUndefined" in repr_str or "MISSING" in repr_str:
        return None
    try:
        json.dumps(default)
        return default
    except (TypeError, ValueError):
        return None


# ── Route extraction ─────────────────────────────────────────────────────


def _classify_response_type(response_model: Any) -> dict:
    """Classify response type to match Rust ResponseType enum."""
    if response_model is None:
        return "RawResponse"
    if _is_streaming_response(response_model):
        return "StreamingResponse"
    return {
        "Model": {
            "qualname": _type_qualname(response_model),
            "status_code": 200,
        }
    }


def _classify_handler_kind(endpoint: Any) -> str:
    """Classify handler kind to match Rust HandlerKind enum."""
    annotations = getattr(endpoint, "__annotations__", {})
    ret = annotations.get("return")
    if ret is not None and _is_streaming_response(ret):
        return "SSE"
    if inspect.isasyncgenfunction(endpoint):
        return "SSE"
    return "RequestResponse"


def _classify_dispatch(endpoint: Any, dependant: Any) -> str:
    """Classify dispatch strategy to match Rust DispatchStrategy enum."""
    has_deps = len(getattr(dependant, "dependencies", [])) > 0
    kind = _classify_handler_kind(endpoint)
    has_request = bool(
        getattr(dependant, "request_param_name", None)
        or getattr(dependant, "response_param_name", None)
        or getattr(dependant, "background_tasks_param_name", None)
    )
    if has_deps or kind != "RequestResponse" or has_request:
        return "AsgiBridge"
    return "Direct"


def _param_source_str(attr_name: str) -> str:
    """Map FastAPI dependant attribute name to Rust ParamSource variant."""
    return {
        "path_params": "Path",
        "query_params": "Query",
        "header_params": "Header",
        "cookie_params": "Cookie",
        "body_params": "Body",
    }[attr_name]


def _extract_params(dependant: Any) -> list[dict]:
    """Extract parameters from a FastAPI Dependant object."""
    params = []
    groups = ["path_params", "query_params", "header_params", "cookie_params", "body_params"]
    for group_name in groups:
        source = _param_source_str(group_name)
        for field in getattr(dependant, group_name, []):
            name = getattr(field, "name", "")
            alias = getattr(field, "alias", None)
            required = getattr(field, "required", True)
            type_ = getattr(field, "type_", None)
            default_json = _default_to_json(field)

            param: dict[str, Any] = {
                "name": name,
                "source": source,
                "type_qualname": _type_qualname(type_),
                "required": required,
            }
            if alias and alias != name:
                param["alias"] = alias
            if default_json is not None:
                param["default_json"] = default_json
            params.append(param)
    return params


def _extract_route(route: Any) -> list[dict]:
    """Extract RouteManifest dicts for a single APIRoute (one per method)."""
    path = getattr(route, "path", "")
    methods = getattr(route, "methods", set()) or set()
    status_code = getattr(route, "status_code", 200) or 200
    tags = list(getattr(route, "tags", []) or [])
    summary = getattr(route, "summary", None)
    description = getattr(route, "description", None)
    deprecated = getattr(route, "deprecated", False) or False
    include_in_schema = getattr(route, "include_in_schema", True)
    if include_in_schema is None:
        include_in_schema = True
    operation_id = getattr(route, "operation_id", None)

    endpoint = getattr(route, "endpoint", None)
    dependant = getattr(route, "dependant", None)
    response_model = getattr(route, "response_model", None)

    handler_qualname = _qualname(endpoint) if endpoint else "unknown"
    kind = _classify_handler_kind(endpoint) if endpoint else "RequestResponse"
    dispatch = _classify_dispatch(endpoint, dependant) if endpoint and dependant else "Direct"
    response_type = _classify_response_type(response_model)
    params = _extract_params(dependant) if dependant else []

    dep_plan = _compile_dependency_plan(dependant) if dependant else None

    manifests = []
    for method_str in sorted(methods):
        manifest: dict[str, Any] = {
            "kind": kind,
            "method": _method_from_str(method_str),
            "path": path,
            "handler_qualname": handler_qualname,
            "params": params,
            "response_type": response_type,
            "tags": tags,
            "dispatch_strategy": dispatch,
            "status_code": status_code,
            "include_in_schema": include_in_schema,
            "deprecated": deprecated,
        }
        if summary is not None:
            manifest["summary"] = summary
        if description is not None:
            manifest["description"] = description
        if operation_id is not None:
            manifest["operation_id"] = operation_id
        if dep_plan is not None:
            manifest["dependency_plan"] = dep_plan
        manifests.append(manifest)
    return manifests


# ── Dependency plan compilation ──────────────────────────────────────────


_MAX_DEPTH = 64


def _compile_dependency_plan(dependant: Any) -> dict | None:
    """Compile a DependencyPlan from a FastAPI Dependant."""
    if dependant is None:
        return None

    dependencies = getattr(dependant, "dependencies", [])
    if not dependencies and not _has_any_params(dependant):
        return None

    steps: list[dict] = []
    seen: set[str] = set()
    generator_indices: list[int] = []
    needs_asgi = False

    # Extract param steps first
    _add_param_steps(dependant, steps)

    # Walk dependency tree
    needs_asgi = _walk_dependencies(dependant, steps, seen, generator_indices, 0)

    # Determine handler kwargs
    handler_kwargs = _get_handler_kwargs(dependant)

    # Check for request/response params
    if (
        getattr(dependant, "request_param_name", None)
        or getattr(dependant, "response_param_name", None)
        or getattr(dependant, "background_tasks_param_name", None)
    ):
        needs_asgi = True

    return {
        "steps": steps,
        "handler_kwargs": handler_kwargs,
        "needs_asgi": needs_asgi,
        "generator_cleanup_indices": generator_indices,
    }


def _has_any_params(dependant: Any) -> bool:
    """Check if a Dependant has any parameter groups."""
    groups = ["path_params", "query_params", "header_params", "cookie_params", "body_params"]
    return any(len(getattr(dependant, g, [])) > 0 for g in groups)


def _add_param_steps(dependant: Any, steps: list[dict]) -> None:
    """Add parameter extraction steps from a Dependant."""
    for field in getattr(dependant, "path_params", []):
        steps.append({"ExtractPath": {
            "name": getattr(field, "name", ""),
            "type_qualname": _type_qualname(getattr(field, "type_", None)),
        }})
    for field in getattr(dependant, "query_params", []):
        step: dict[str, Any] = {
            "name": getattr(field, "name", ""),
            "type_qualname": _type_qualname(getattr(field, "type_", None)),
            "required": getattr(field, "required", True),
        }
        default = _default_to_json(field)
        if default is not None:
            step["default_json"] = default
        steps.append({"ExtractQuery": step})
    for field in getattr(dependant, "header_params", []):
        name = getattr(field, "name", "")
        alias = getattr(field, "alias", name) or name
        steps.append({"ExtractHeader": {
            "name": name,
            "alias": alias.lower().replace("_", "-"),
            "type_qualname": _type_qualname(getattr(field, "type_", None)),
            "required": getattr(field, "required", True),
        }})
    for field in getattr(dependant, "cookie_params", []):
        steps.append({"ExtractCookie": {
            "name": getattr(field, "name", ""),
            "type_qualname": _type_qualname(getattr(field, "type_", None)),
            "required": getattr(field, "required", True),
        }})
    for field in getattr(dependant, "body_params", []):
        steps.append({"ValidateBody": {
            "name": getattr(field, "name", ""),
            "model_qualname": _type_qualname(getattr(field, "type_", None)),
        }})


def _walk_dependencies(
    dependant: Any,
    steps: list[dict],
    seen: set[str],
    generator_indices: list[int],
    depth: int,
) -> bool:
    """Recursively walk Dependant.dependencies and add CallPython steps."""
    if depth > _MAX_DEPTH:
        return False

    needs_asgi = False
    for sub_dep in getattr(dependant, "dependencies", []):
        sub_dependant = getattr(sub_dep, "dependency", None)
        if sub_dependant is None:
            continue

        call = getattr(sub_dep, "call", None) or sub_dependant
        qn = _qualname(call)

        if qn in seen:
            continue
        seen.add(qn)

        # Recurse into sub-dependency's own dependencies
        sub_dep_dependant = getattr(sub_dep, "dependency", None)
        if sub_dep_dependant and hasattr(sub_dep_dependant, "dependencies"):
            child_asgi = _walk_dependencies(sub_dep_dependant, steps, seen, generator_indices, depth + 1)
            needs_asgi = needs_asgi or child_asgi

        is_gen = inspect.isasyncgenfunction(call) or inspect.isgeneratorfunction(call)
        is_async = inspect.iscoroutinefunction(call) or inspect.isasyncgenfunction(call)
        use_cache = getattr(sub_dep, "use_cache", True)

        # Determine inputs from sub-dependency params
        inputs: list[str] = []
        if hasattr(sub_dep, "dependencies"):
            for inner in getattr(sub_dep, "dependencies", []):
                inner_call = getattr(inner, "call", None) or getattr(inner, "dependency", None)
                if inner_call:
                    # Use parameter name that maps to this dependency
                    param_name = getattr(inner, "name", None) or _qualname(inner_call).rsplit(".", 1)[-1]
                    inputs.append(param_name)

        target_kwarg = getattr(sub_dep, "name", None) or qn.rsplit(".", 1)[-1]

        step_idx = len(steps)
        steps.append({"CallPython": {
            "dep_qualname": qn,
            "target_kwarg": target_kwarg,
            "inputs": inputs,
            "is_generator": is_gen,
            "is_async": is_async,
            "use_cache": use_cache,
        }})
        if is_gen:
            generator_indices.append(step_idx)

    return needs_asgi


def _get_handler_kwargs(dependant: Any) -> list[str]:
    """Determine handler kwarg names from the Dependant."""
    call = getattr(dependant, "call", None)
    if call is None:
        return []
    try:
        sig = inspect.signature(call)
        return list(sig.parameters.keys())
    except (ValueError, TypeError):
        return []


# ── Dependency graph ─────────────────────────────────────────────────────


def _build_dependency_graph(app: Any) -> list[dict]:
    """Build app-wide dependency graph from all routes."""
    from fastapi.routing import APIRoute

    nodes: dict[str, dict] = {}
    for route in getattr(app, "routes", []):
        if not isinstance(route, APIRoute):
            continue
        dependant = getattr(route, "dependant", None)
        if dependant is None:
            continue
        _collect_deps(dependant, nodes, depth=0)

    return list(nodes.values())


def _collect_deps(dependant: Any, nodes: dict[str, dict], depth: int) -> None:
    """Recursively collect dependency nodes."""
    if depth > _MAX_DEPTH:
        return
    for sub_dep in getattr(dependant, "dependencies", []):
        call = getattr(sub_dep, "call", None) or getattr(sub_dep, "dependency", None)
        if call is None:
            continue

        qn = _qualname(call)
        if qn in nodes:
            continue

        is_gen = inspect.isasyncgenfunction(call) or inspect.isgeneratorfunction(call)
        is_async = inspect.iscoroutinefunction(call) or inspect.isasyncgenfunction(call)
        sub_deps: list[str] = []

        sub_dependant = getattr(sub_dep, "dependency", None)
        if sub_dependant and hasattr(sub_dependant, "dependencies"):
            for inner in getattr(sub_dependant, "dependencies", []):
                inner_call = getattr(inner, "call", None) or getattr(inner, "dependency", None)
                if inner_call:
                    sub_deps.append(_qualname(inner_call))

        params = _extract_params(sub_dep) if hasattr(sub_dep, "path_params") else []

        nodes[qn] = {
            "qualname": qn,
            "tier": "Standard",
            "scope": "Request",
            "is_generator": is_gen,
            "is_async": is_async,
            "sub_dependencies": sub_deps,
            "params": params,
        }

        if sub_dependant and hasattr(sub_dependant, "dependencies"):
            _collect_deps(sub_dependant, nodes, depth + 1)


# ── Lifecycle deps ───────────────────────────────────────────────────────


def _extract_lifecycle_deps(app: Any) -> list[dict]:
    """Identify lifecycle deps (async generators that yield)."""
    from fastapi.routing import APIRoute

    lifecycle: dict[str, dict] = {}
    order = 0
    for route in getattr(app, "routes", []):
        if not isinstance(route, APIRoute):
            continue
        dependant = getattr(route, "dependant", None)
        if dependant is None:
            continue
        for sub_dep in getattr(dependant, "dependencies", []):
            call = getattr(sub_dep, "call", None) or getattr(sub_dep, "dependency", None)
            if call is None:
                continue
            if not (inspect.isasyncgenfunction(call) or inspect.isgeneratorfunction(call)):
                continue
            qn = _qualname(call)
            if qn in lifecycle:
                continue
            lifecycle[qn] = {
                "qualname": qn,
                "init_order": order,
                "shutdown_order": 0,
                "scope": "Request",
            }
            order += 1

    # Compute shutdown order (reverse of init)
    entries = sorted(lifecycle.values(), key=lambda e: e["init_order"])
    total = len(entries)
    for entry in entries:
        entry["shutdown_order"] = total - 1 - entry["init_order"]

    return entries


# ── Manifest meta ────────────────────────────────────────────────────────


def _build_meta(app_module: str) -> dict:
    """Build ManifestMeta dict."""
    apx_version = _get_apx_version()
    python_version = f"{sys.version_info.major}.{sys.version_info.minor}.{sys.version_info.micro}"
    fastapi_version = _get_package_version("fastapi")
    timestamp = datetime.now(timezone.utc).isoformat()

    meta: dict[str, Any] = {
        "apx_version": apx_version,
        "python_version": python_version,
        "build_timestamp": timestamp,
        "app_module": app_module,
    }
    if fastapi_version is not None:
        meta["fastapi_version"] = fastapi_version
    return meta


def _get_apx_version() -> str:
    """Get the installed apx version."""
    try:
        from importlib.metadata import version
        return version("apx")
    except Exception:
        return "0.0.0"


def _get_package_version(package: str) -> str | None:
    """Get an installed package version, or None."""
    try:
        from importlib.metadata import version
        return version(package)
    except Exception:
        return None


# ── Main entry point ─────────────────────────────────────────────────────


def _find_fastapi_app(module: Any) -> Any:
    """Find a FastAPI instance in a module (same logic as Rust discovery)."""
    try:
        from fastapi import FastAPI
    except ImportError as e:
        raise RuntimeError(f"FastAPI not installed: {e}") from e

    # Check conventional `app` attribute first
    app = getattr(module, "app", None)
    if isinstance(app, FastAPI):
        return app

    # Walk all non-dunder attributes
    for name in dir(module):
        if name.startswith("_"):
            continue
        attr = getattr(module, name, None)
        if isinstance(attr, FastAPI):
            return attr

    raise RuntimeError(f"no FastAPI instance found in {module.__name__}")


def compile_manifest(app_module: str) -> dict:
    """Import module, find FastAPI app, extract full manifest dict."""
    # Ensure src/ is on path
    src = os.path.join(os.getcwd(), "src")
    if os.path.isdir(src) and src not in sys.path:
        sys.path.insert(0, src)
    cwd = os.getcwd()
    if cwd not in sys.path:
        sys.path.insert(0, cwd)

    from fastapi.routing import APIRoute

    module = importlib.import_module(app_module)
    app = _find_fastapi_app(module)

    # Extract routes
    routes: list[dict] = []
    for route in getattr(app, "routes", []):
        if isinstance(route, APIRoute):
            routes.extend(_extract_route(route))

    # Build dependency graph
    dep_graph = _build_dependency_graph(app)

    # Extract lifecycle deps
    lifecycle_deps = _extract_lifecycle_deps(app)

    # Get OpenAPI schema
    openapi_schema = None
    try:
        openapi_schema = app.openapi()
    except Exception:
        pass

    # Build metadata
    meta = _build_meta(app_module)

    manifest: dict[str, Any] = {
        "meta": meta,
        "routes": routes,
        "dependency_graph": dep_graph,
        "lifecycle_deps": lifecycle_deps,
        "max_body_limit": 1048576,
        "validation_results": [],
    }
    if openapi_schema is not None:
        manifest["openapi_schema"] = openapi_schema

    return manifest


def main() -> None:
    """CLI entry point: extract manifest and print JSON to stdout."""
    if len(sys.argv) < 2:
        print("usage: python -m apx._manifest <app_module>", file=sys.stderr)
        sys.exit(1)

    app_module = sys.argv[1]
    manifest = compile_manifest(app_module)
    print(json.dumps(manifest, indent=2, default=str))


if __name__ == "__main__":
    main()
