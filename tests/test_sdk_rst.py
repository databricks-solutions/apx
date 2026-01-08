"""Tests for RST documentation parsing and SDK enrichment."""

import pytest

from apx.mcp.sdk import (
    _extract_methods_from_sdk,
    get_cache_path,
    get_installed_sdk_version,
    initialize_sdk_cache,
    is_cached,
)
from apx.mcp.sdk_parser import (
    enrich_method_with_rst,
    load_all_rst_docs,
    parse_rst_file,
)


@pytest.fixture(scope="module")
def sdk_version():
    """Get the installed SDK version."""
    version = get_installed_sdk_version()
    if version is None:
        pytest.skip("databricks-sdk not installed")
    return version


@pytest.fixture(scope="module")
def sdk_cache(sdk_version):
    """Initialize and return SDK cache information."""
    cache_info = initialize_sdk_cache()
    if not cache_info.get("cached"):
        pytest.skip("SDK cache not available")
    return cache_info


@pytest.fixture(scope="module")
def rst_docs(sdk_version, sdk_cache):
    """Load all RST documentation."""
    if not is_cached(sdk_version):
        pytest.skip("SDK cache not available")

    docs_path = get_cache_path(sdk_version) / "docs"
    return load_all_rst_docs(docs_path)


@pytest.fixture(scope="module")
def sdk_methods():
    """Extract all SDK methods."""
    return _extract_methods_from_sdk()


class TestRSTParser:
    """Tests for RST file parsing."""

    def test_parse_rst_file(self, sdk_version, sdk_cache):
        """Test parsing a single RST file."""
        docs_path = get_cache_path(sdk_version) / "docs" / "workspace"

        # Find a sample RST file
        sample_file = None
        for service_dir in docs_path.iterdir():
            if service_dir.is_dir():
                for rst_file in service_dir.glob("*.rst"):
                    if rst_file.stem != "index":
                        sample_file = rst_file
                        break
                if sample_file:
                    break

        if sample_file is None:
            pytest.skip("No RST files found")

        doc = parse_rst_file(sample_file)
        assert doc is not None, "Failed to parse RST file"
        assert doc.service_name, "Service name should not be empty"
        assert doc.class_name, "Class name should not be empty"

    def test_rst_docs_loaded(self, rst_docs):
        """Test that RST documentation is loaded."""
        assert len(rst_docs) > 0, "No RST docs loaded"
        assert len(rst_docs) > 100, f"Expected >100 service docs, got {len(rst_docs)}"

    def test_rst_methods_count(self, rst_docs):
        """Test that RST docs contain methods."""
        total_methods = sum(len(doc.methods) for doc in rst_docs.values())
        assert total_methods > 1000, (
            f"Expected >1000 methods in RST, got {total_methods}"
        )


class TestSDKEnrichment:
    """Tests for SDK method enrichment with RST documentation."""

    def test_sdk_methods_extracted(self, sdk_methods):
        """Test that SDK methods are extracted."""
        assert len(sdk_methods) > 0, "No SDK methods extracted"
        assert len(sdk_methods) > 500, f"Expected >500 methods, got {len(sdk_methods)}"

    def test_enrichment_coverage(self, sdk_methods, rst_docs):
        """Test that RST enrichment achieves high coverage."""
        enriched_count = 0

        for method in sdk_methods:
            enriched = enrich_method_with_rst(method, rst_docs)
            if enriched.has_rst:
                enriched_count += 1

        coverage = (enriched_count / len(sdk_methods)) * 100

        # Assert coverage is at least 90%
        assert coverage >= 90.0, (
            f"RST coverage too low: {enriched_count}/{len(sdk_methods)} ({coverage:.1f}%) "
            f"- expected at least 90%"
        )

    def test_enrichment_quality(self, sdk_methods, rst_docs):
        """Test that enriched methods have quality RST documentation."""
        enriched_methods = []

        for method in sdk_methods[:100]:  # Test first 100 methods
            enriched = enrich_method_with_rst(method, rst_docs)
            if enriched.has_rst:
                enriched_methods.append(enriched)

        assert len(enriched_methods) > 0, "No methods were enriched"

        # Check quality of enrichment
        for method in enriched_methods[:10]:  # Check first 10 enriched
            assert method.rst_docs is not None, "RST docs should not be None"
            assert len(method.rst_docs) > 50, "RST docs should have substantial content"
            assert "py:method::" in method.rst_docs, (
                "RST docs should contain method directive"
            )

    def test_service_coverage(self, sdk_methods, rst_docs):
        """Test coverage by service."""
        service_stats = {}

        for method in sdk_methods:
            if method.service_name not in service_stats:
                service_stats[method.service_name] = {"total": 0, "with_rst": 0}
            service_stats[method.service_name]["total"] += 1

            enriched = enrich_method_with_rst(method, rst_docs)
            if enriched.has_rst:
                service_stats[method.service_name]["with_rst"] += 1

        # Find services with 100% coverage
        full_coverage_services = [
            service
            for service, stats in service_stats.items()
            if stats["total"] > 0 and stats["with_rst"] == stats["total"]
        ]

        # Assert that we have many services with 100% coverage
        assert len(full_coverage_services) > 50, (
            f"Expected >50 services with 100% coverage, got {len(full_coverage_services)}"
        )

    def test_ext_class_mapping(self, sdk_methods, rst_docs):
        """Test that Ext classes are properly mapped to their base classes."""
        ext_methods = [m for m in sdk_methods if m.class_name.endswith("Ext")]

        if not ext_methods:
            pytest.skip("No Ext classes found")

        # Test that Ext classes can be enriched
        enriched_ext_count = sum(
            1 for m in ext_methods if enrich_method_with_rst(m, rst_docs).has_rst
        )

        ext_coverage = (enriched_ext_count / len(ext_methods)) * 100

        # Ext classes should have high coverage too
        assert ext_coverage > 80.0, (
            f"Ext class coverage too low: {enriched_ext_count}/{len(ext_methods)} "
            f"({ext_coverage:.1f}%)"
        )


class TestRSTContent:
    """Tests for RST content quality."""

    def test_method_signatures(self, rst_docs):
        """Test that RST docs contain method signatures."""
        has_signatures = False

        for doc in list(rst_docs.values())[:10]:
            for method in doc.methods.values():
                if "py:method::" in method.full_text:
                    has_signatures = True
                    break
            if has_signatures:
                break

        assert has_signatures, "RST docs should contain method signatures"

    def test_parameter_documentation(self, rst_docs):
        """Test that RST docs contain parameter documentation."""
        has_params = False

        for doc in list(rst_docs.values())[:20]:
            for method in doc.methods.values():
                if ":param" in method.full_text:
                    has_params = True
                    break
            if has_params:
                break

        assert has_params, "RST docs should contain parameter documentation"


class TestCacheManagement:
    """Tests for SDK cache management."""

    def test_cache_initialization(self, sdk_version):
        """Test that cache can be initialized."""
        cache_info = initialize_sdk_cache()
        assert "status" in cache_info
        assert (
            cache_info.get("version") == sdk_version
            or cache_info.get("status") == "failed"
        )

    def test_cache_location(self, sdk_version, sdk_cache):
        """Test that cache is in the expected location."""
        cache_path = get_cache_path(sdk_version)
        assert cache_path.exists(), f"Cache path should exist: {cache_path}"

        docs_path = cache_path / "docs"
        assert docs_path.exists(), f"Docs path should exist: {docs_path}"

        workspace_path = docs_path / "workspace"
        assert workspace_path.exists(), f"Workspace path should exist: {workspace_path}"

    def test_is_cached(self, sdk_version):
        """Test cache detection."""
        assert is_cached(sdk_version), "SDK version should be cached"
