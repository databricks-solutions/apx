from typing import Any
from hatchling.builders.hooks.plugin.interface import BuildHookInterface
from pathlib import Path
import subprocess
import shutil
from hatchling.plugin import hookimpl


class ApxHook(BuildHookInterface):
    PLUGIN_NAME = "apx"

    def initialize(self, version: str, build_data: dict[str, Any]) -> None:
        self.app.display_info(
            f"Running build hook for project {self.metadata.name} in directory {Path.cwd()}"
        )

        process = subprocess.Popen(
            ["bun", "run", "build"],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )

        if process.stdout:
            for line in process.stdout:
                self.app.display_info(line, end="")

        if process.stderr:
            for line in process.stderr:
                self.app.display_error(line, end="")

        process.wait()

    def finalize(self, version, build_data, artifact_path):
        self.app.display_info(
            f"Finalizing build hook for project {self.metadata.name} in directory {Path.cwd()} with artifact path {artifact_path}"
        )
        # cleanup the .build directory
        build_path = self.config.get("build_path", ".build")
        build_dir = Path.cwd() / build_path
        if build_dir.exists():
            self.app.display_info(f"Removing {build_dir}")
            shutil.rmtree(build_dir)

        # if app.yml or app.yaml exists in cwd, copy it to the .build directory
        app_file_options = ["app.yml", "app.yaml"]
        for app_file in app_file_options:
            if (Path.cwd() / app_file).exists():
                self.app.display_info(f"Copying {app_file} to {build_dir}")
                shutil.copy(Path.cwd() / app_file, build_dir / app_file)
                break

        # copy the build artifacts to the .build directory
        self.app.display_info(f"Copying build artifacts to {build_dir}")
        artifact_path = Path(artifact_path)
        shutil.copytree(artifact_path.parent, build_dir)

        # write .build/requirements.txt with artifact path
        reqs_file = build_dir / "requirements.txt"
        reqs_file.write_text(f"{artifact_path.name}\n")

        self.app.display_info(f"Build dir {build_dir} is ready for deployment")


@hookimpl
def hatch_register_environment():
    return ApxHook
