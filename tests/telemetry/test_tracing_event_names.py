"""Enforcement test: every tracing macro in the framework crate must have ``name:``.

This prevents regressions when new tracing calls are added without an
explicit event name.  The ``opentelemetry-appender-tracing`` bridge maps
``tracing::Metadata::name()`` to the OTEL ``event_name`` field; without an
explicit ``name:`` the bridge emits an auto-generated string like
``"event crates/framework/src/foo.rs:42"`` which is useless.
"""

from __future__ import annotations

import re
from pathlib import Path

FRAMEWORK_SRC = Path(__file__).resolve().parents[2] / "crates" / "framework" / "src"

TRACING_MACRO_RE = re.compile(
    r"\b(?:tracing::)?(?:info|warn|error|debug|trace)!\(",
)

SKIP_PATTERNS = (
    "compile_error!",
    "//",
    "///",
    "//!",
)


def _collect_missing_name() -> list[str]:
    """Return ``file:line`` for tracing calls missing ``name:``."""
    violations: list[str] = []

    for rs_file in sorted(FRAMEWORK_SRC.rglob("*.rs")):
        lines = rs_file.read_text().splitlines()
        i = 0
        while i < len(lines):
            line = lines[i]
            stripped = line.lstrip()

            if any(stripped.startswith(p) for p in SKIP_PATTERNS):
                i += 1
                continue

            if not TRACING_MACRO_RE.search(line):
                i += 1
                continue

            has_name = "name:" in line
            if not has_name and i + 1 < len(lines):
                has_name = "name:" in lines[i + 1]

            if not has_name:
                rel = rs_file.relative_to(FRAMEWORK_SRC)
                violations.append(f"{rel}:{i + 1}")

            i += 1

    return violations


def test_all_framework_tracing_macros_have_name() -> None:
    """Every tracing macro in crates/framework/src/ must have ``name:``."""
    violations = _collect_missing_name()
    assert not violations, (
        f"{len(violations)} tracing macro(s) missing `name:` in framework crate:\n"
        + "\n".join(f"  {v}" for v in violations)
    )
