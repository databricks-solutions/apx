import os
import shutil
import subprocess
from pathlib import Path
from typing import Any

from hatchling.builders.hooks.plugin.interface import BuildHookInterface


class PackHook(BuildHookInterface):
    def initialize(self, version: str, build_data: dict[str, Any]) -> None:
        filename = Path("src/apx/dist") / f"apx-plugin.tgz"
        self.app.display_info(f"Packing {version} into {filename.resolve()}")
        # run bun run build in the project root
        subprocess.run(["bun", "run", "build"])
        # run bun pm pack in the project root
        subprocess.run(["bun", "pm", "pack", "--filename", str(filename.resolve())])

        self.app.display_info(f"Packed plugin into {filename.resolve()}")
