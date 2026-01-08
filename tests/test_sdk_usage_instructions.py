"""Tests for SDK usage instructions tool."""

import pytest

from apx.mcp.sdk import (
    get_installed_sdk_version,
    get_sdk_usage_instructions,
    initialize_sdk_cache,
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
    """Initialize SDK cache."""
    cache_info = initialize_sdk_cache()
    if not cache_info.get("cached"):
        pytest.skip("SDK cache not available")
    return cache_info


class TestUsageInstructions:
    """Tests for SDK usage instructions tool."""

    @pytest.mark.asyncio
    async def test_get_usage_instructions(self, sdk_version, sdk_cache):
        """Test that usage instructions can be retrieved."""
        result = await get_sdk_usage_instructions()

        assert result is not None, "Result should not be None"
        assert result.pagination_guide, "Pagination guide should not be empty"
        assert result.long_running_operations_guide, "Wait guide should not be empty"
        assert result.custom_instructions, "Custom instructions should not be empty"

    @pytest.mark.asyncio
    async def test_pagination_guide_content(self, sdk_version, sdk_cache):
        """Test that pagination guide has expected content."""
        result = await get_sdk_usage_instructions()

        assert "Paginated responses" in result.pagination_guide
        assert "Iterator[T]" in result.pagination_guide
        assert "Python" in result.pagination_guide

    @pytest.mark.asyncio
    async def test_wait_guide_content(self, sdk_version, sdk_cache):
        """Test that wait guide has expected content."""
        result = await get_sdk_usage_instructions()

        assert "Long-running operations" in result.long_running_operations_guide
        assert "Wait" in result.long_running_operations_guide
        assert "result()" in result.long_running_operations_guide

    @pytest.mark.asyncio
    async def test_custom_instructions_present(self, sdk_version, sdk_cache):
        """Test that custom instructions are present."""
        result = await get_sdk_usage_instructions()

        assert len(result.custom_instructions) > 0
        assert "Custom Usage Instructions" in result.custom_instructions

    @pytest.mark.asyncio
    async def test_guides_have_examples(self, sdk_version, sdk_cache):
        """Test that guides contain code examples."""
        result = await get_sdk_usage_instructions()

        # Pagination guide should have code examples
        assert "```python" in result.pagination_guide
        assert "WorkspaceClient" in result.pagination_guide

        # Wait guide should have code examples
        assert "```python" in result.long_running_operations_guide
        assert "datetime.timedelta" in result.long_running_operations_guide
