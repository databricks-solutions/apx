from importlib import resources
from pathlib import Path

from apx import get_bun_binary_path


def test_bun_binary_path_exists() -> None:
    bun_path = get_bun_binary_path()
    assert bun_path.exists()
    assert bun_path.is_file()

    expected = resources.files("apx").joinpath("binaries", bun_path.name)
    expected_path = Path(str(expected))
    assert bun_path.resolve() == expected_path.resolve()
