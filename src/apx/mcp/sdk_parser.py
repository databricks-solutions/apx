"""RST documentation parser for Databricks SDK.

Parses RST files from the SDK docs/ folder to extract rich documentation
including usage examples and detailed method descriptions.
"""

import re
from pathlib import Path
from typing import TYPE_CHECKING

from docutils import nodes
from docutils.core import publish_doctree
from pydantic import BaseModel

if TYPE_CHECKING:
    from apx.mcp.models import SDKMethodSpec


class RSTMethodDoc(BaseModel):
    """Parsed documentation for a method from RST files."""

    method_name: str
    description: str
    parameters: list[str]
    full_text: str


class RSTServiceDoc(BaseModel):
    """Parsed documentation for a service/API class from RST files."""

    service_name: str
    class_name: str
    class_description: str
    methods: dict[str, RSTMethodDoc]


def _extract_text_from_node(node: nodes.Node) -> str:
    """Extract plain text from a docutils node."""
    if isinstance(node, nodes.Text):
        return str(node)
    elif hasattr(node, "children"):
        return "".join(_extract_text_from_node(child) for child in node.children)
    return ""


def _parse_py_method_directive(node: nodes.Node) -> RSTMethodDoc | None:
    """Parse a py:method directive to extract method documentation.

    Expected format:
    .. py:method:: method_name(param1: Type1, param2: Type2) -> ReturnType

        Description text here.

        :param param1: Description
        :param param2: Description
    """
    # Find the signature (first line of the directive)
    signature = ""
    description_parts: list[str] = []
    parameters: list[str] = []

    # Get node types safely (docutils types may not have stubs)
    desc_signature_type = getattr(nodes, "desc_signature", None)
    desc_content_type = getattr(nodes, "desc_content", None)

    # Look for the signature in the directive
    for child in node.children:
        if desc_signature_type and isinstance(child, desc_signature_type):
            signature = _extract_text_from_node(child).strip()
        elif desc_content_type and isinstance(child, desc_content_type):
            # Extract content (description and parameters)
            for content_child in child.children:
                if isinstance(content_child, nodes.paragraph):
                    text = _extract_text_from_node(content_child).strip()
                    description_parts.append(text)
                elif isinstance(content_child, nodes.field_list):
                    # Extract parameter documentation
                    for field in content_child.children:
                        if isinstance(field, nodes.field):
                            field_text = _extract_text_from_node(field).strip()
                            if field_text.startswith(":param"):
                                parameters.append(field_text)

    if not signature:
        return None

    # Extract method name from signature
    method_name_match = re.match(r"^(\w+)\s*\(", signature)
    if not method_name_match:
        return None

    method_name = method_name_match.group(1)
    description = "\n".join(description_parts)
    full_text = f"{signature}\n\n{description}"
    if parameters:
        full_text += "\n\nParameters:\n" + "\n".join(parameters)

    return RSTMethodDoc(
        method_name=method_name,
        description=description,
        parameters=parameters,
        full_text=full_text,
    )


def _fallback_parse_rst_content(content: str) -> dict[str, RSTMethodDoc]:
    """Fallback regex-based parser for RST content.

    Used when docutils parsing fails or for simpler parsing needs.
    """
    methods: dict[str, RSTMethodDoc] = {}

    # Pattern to match py:method directives
    # .. py:method:: method_name(params) -> return
    method_pattern = re.compile(
        r"\.\.\s+py:method::\s+(\w+)\s*\([^)]*\)(?:\s*->\s*[^\n]+)?",
        re.MULTILINE,
    )

    # Find all method declarations
    matches = list(method_pattern.finditer(content))

    for i, match in enumerate(matches):
        method_name = match.group(1)
        method_start = match.start()

        # Find the end of this method's documentation (next method or end of file)
        method_end = matches[i + 1].start() if i + 1 < len(matches) else len(content)
        method_text = content[method_start:method_end].strip()

        # Extract the first paragraph after the method signature as description
        lines = method_text.split("\n")
        description_lines: list[str] = []
        params: list[str] = []

        in_description = False
        for line in lines[1:]:  # Skip the first line (signature)
            stripped = line.strip()

            if not stripped:
                if in_description:
                    break  # Empty line after description
                continue

            if stripped.startswith(":param"):
                params.append(stripped)
            elif not stripped.startswith(":"):
                in_description = True
                description_lines.append(stripped)

        description = " ".join(description_lines)

        methods[method_name] = RSTMethodDoc(
            method_name=method_name,
            description=description,
            parameters=params,
            full_text=method_text,
        )

    return methods


def parse_rst_file(rst_path: Path) -> RSTServiceDoc | None:
    """Parse an RST file to extract service and method documentation.

    Args:
        rst_path: Path to the RST file (e.g., docs/workspace/apps/apps.rst)

    Returns:
        RSTServiceDoc with parsed documentation, or None if parsing fails
    """
    if not rst_path.exists():
        return None

    content = rst_path.read_text(encoding="utf-8")

    # Extract service name from the file path
    # e.g., docs/workspace/apps/apps.rst -> apps
    service_name = rst_path.parent.name

    # Try to parse with docutils first
    try:
        doctree = publish_doctree(content)

        # Extract class name and description from currentmodule and py:class directives
        class_name = ""
        class_description = ""
        methods: dict[str, RSTMethodDoc] = {}

        # Look for the module directive
        module_match = re.search(r"\.\.\s+currentmodule::\s+([\w.]+)", content)
        if module_match:
            module_path = module_match.group(1)
            # Extract service name from module path (e.g., databricks.sdk.service.apps -> apps)
            service_name = module_path.split(".")[-1]

        # Look for the class directive and description
        class_match = re.search(
            r"\.\.\s+py:class::\s+(\w+)\s*\n\s*\n\s+(.+?)(?=\n\s+\.\.|$)",
            content,
            re.DOTALL,
        )
        if class_match:
            class_name = class_match.group(1)
            class_description = class_match.group(2).strip()

        # Walk the doctree to find method directives
        desc_type = getattr(nodes, "desc", None)
        for node in doctree.traverse():
            if (
                desc_type
                and isinstance(node, desc_type)
                and node.get("desctype") == "method"
            ):
                method_doc = _parse_py_method_directive(node)
                if method_doc:
                    methods[method_doc.method_name] = method_doc

        # If docutils didn't find methods, fall back to regex
        if not methods:
            methods = _fallback_parse_rst_content(content)

        if not class_name:
            # Try to extract from filename
            class_name = rst_path.stem.title() + "API"

        return RSTServiceDoc(
            service_name=service_name,
            class_name=class_name,
            class_description=class_description,
            methods=methods,
        )

    except Exception:
        # Fallback to regex parsing if docutils fails
        try:
            # Extract class info with regex
            class_match = re.search(
                r"\.\.\s+py:class::\s+(\w+)\s*\n\s*\n\s+(.+?)(?=\n\s+\.\.|$)",
                content,
                re.DOTALL,
            )

            class_name = ""
            class_description = ""

            if class_match:
                class_name = class_match.group(1)
                class_description = class_match.group(2).strip()
            else:
                class_name = rst_path.stem.title() + "API"

            methods = _fallback_parse_rst_content(content)

            return RSTServiceDoc(
                service_name=service_name,
                class_name=class_name,
                class_description=class_description,
                methods=methods,
            )
        except Exception:
            return None


def load_all_rst_docs(docs_path: Path) -> dict[str, RSTServiceDoc]:
    """Load all RST documentation from the docs/workspace directory.

    Args:
        docs_path: Path to the docs folder (e.g., ~/.apx/caches/sdk/0.77.0/docs)

    Returns:
        Dictionary mapping service module names to their RST documentation
    """
    rst_docs: dict[str, RSTServiceDoc] = {}

    workspace_docs = docs_path / "workspace"
    if not workspace_docs.exists():
        return rst_docs

    # Find all RST files in workspace subdirectories
    for service_dir in workspace_docs.iterdir():
        if not service_dir.is_dir():
            continue

        for rst_file in service_dir.glob("*.rst"):
            # Skip index files
            if rst_file.stem == "index":
                continue

            doc = parse_rst_file(rst_file)
            if doc and doc.methods:
                # Use the service_name from the parsed doc (which comes from the module directive)
                # as well as the file stem as keys to maximize matching
                service_key = doc.service_name
                file_key = rst_file.stem

                # Add using service name from parsed doc
                if service_key in rst_docs:
                    rst_docs[service_key].methods.update(doc.methods)
                else:
                    rst_docs[service_key] = doc

                # Also add using file stem if different
                if file_key != service_key:
                    if file_key in rst_docs:
                        rst_docs[file_key].methods.update(doc.methods)
                    else:
                        # Create a copy with the file stem as service name
                        rst_docs[file_key] = RSTServiceDoc(
                            service_name=file_key,
                            class_name=doc.class_name,
                            class_description=doc.class_description,
                            methods=doc.methods.copy(),
                        )

    return rst_docs


def _get_module_name_from_class(class_name: str) -> str | None:
    """Get module name from API class by introspecting the SDK.

    Args:
        class_name: The API class name (e.g., "AppsAPI", "JobsExt")

    Returns:
        Module name (e.g., "apps", "jobs") or None if not found
    """
    try:
        from databricks.sdk import service

        # Look through service modules for the class
        for module_name in dir(service):
            if module_name.startswith("_"):
                continue

            try:
                module = getattr(service, module_name)
                if hasattr(module, class_name):
                    return module_name
            except Exception:
                continue

        # If not found and class ends with "Ext", try the base class (replace Ext with API)
        if class_name.endswith("Ext"):
            base_class_name = class_name[:-3] + "API"
            for module_name in dir(service):
                if module_name.startswith("_"):
                    continue

                try:
                    module = getattr(service, module_name)
                    if hasattr(module, base_class_name):
                        return module_name
                except Exception:
                    continue
    except Exception:
        pass

    return None


def enrich_method_with_rst(
    method_spec: "SDKMethodSpec", rst_docs: dict[str, RSTServiceDoc]
) -> "SDKMethodSpec":
    """Enrich a method specification with RST documentation.

    Args:
        method_spec: The method specification from SDK introspection
        rst_docs: Dictionary of RST documentation by service name (module name)

    Returns:
        Updated method specification with RST documentation
    """
    # Import at runtime to avoid circular dependency
    from apx.mcp.models import SDKMethodSpec

    # Try to find RST docs using the module name derived from the class
    module_name = _get_module_name_from_class(method_spec.class_name)
    if not module_name:
        return method_spec

    service_doc = rst_docs.get(module_name)
    if not service_doc:
        return method_spec

    method_doc = service_doc.methods.get(method_spec.method_name)
    if not method_doc:
        return method_spec

    # Update the spec with RST documentation
    return SDKMethodSpec(
        service_name=method_spec.service_name,
        class_name=method_spec.class_name,
        method_name=method_spec.method_name,
        full_name=method_spec.full_name,
        signature=method_spec.signature,
        docstring=method_spec.docstring,
        rst_docs=method_doc.full_text,
        has_rst=True,
        parameters=method_spec.parameters,
    )
