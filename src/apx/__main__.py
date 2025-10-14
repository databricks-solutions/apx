from importlib import resources
import importlib
import json
from pathlib import Path
import random
import shutil
import subprocess
from typer import Argument, Exit, Typer, Option
from rich import print
from apx._version import version as apx_version
import jinja2
from fastapi import FastAPI


def version_callback(value: bool):
    if value:
        print(f"apx version: {apx_version}")
        raise Exit(code=0)


app = Typer(
    name="apx | project quickstarter",
)

templates_dir: Path = resources.files("apx").joinpath("templates")  # type: ignore
jinja2_env = jinja2.Environment(loader=jinja2.FileSystemLoader(templates_dir))


@app.callback()
def main(
    version: bool = Option(
        None,
        "--version",
        callback=version_callback,
        is_eager=True,
        help="Show the version and exit.",
    ),
):
    """Project quickstarter CLI."""
    pass


@app.command(name="version", help="Show the version of apx")
def version():
    print(f"apx version: {apx_version}")


def is_uv_installed() -> bool:
    """Check if uv is installed on the system."""
    return shutil.which("uv") is not None


def is_bun_installed() -> bool:
    """Check if bun is installed on the system."""
    return shutil.which("bun") is not None


def random_name():
    # docker-style random name
    adjectives = [
        "fast",
        "simple",
        "clean",
        "elegant",
        "modern",
        "cool",
        "awesome",
        "brave",
        "bold",
        "creative",
        "curious",
        "dynamic",
        "energetic",
        "fantastic",
        "giant",
    ]

    animals = [
        "lion",
        "tiger",
        "bear",
        "wolf",
        "fox",
        "dog",
        "cat",
        "bird",
        "fish",
        "horse",
        "rabbit",
        "snake",
        "turtle",
        "whale",
        "dolphin",
        "shark",
        "octopus",
    ]

    return f"{random.choice(adjectives)}_{random.choice(animals)}"


version_option = Option(
    None,
    "--version",
    help="Show the version of apx",
    callback=version_callback,
    is_eager=True,
)


@app.command(name="init", help="Initialize a new project")
def init(
    app_name: str | None = Option(None, help="The name of the project"),
    app_path: Path | None = Option(
        None,
        help="The path to the app. If not provided, the app will be created in the current working directory",
    ),
    version: bool | None = version_option,
):
    # check if `uv` is installed
    if not is_uv_installed():
        print("uv is not installed. Please install uv to continue.")
        return Exit(code=1)
    # check if `bun` is installed
    if not is_bun_installed():
        print("bun is not installed. Please install bun to continue.")
        return Exit(code=1)

    if app_name is None:
        app_name = random_name()
    else:
        app_name = (
            app_name.lower().replace(" ", "_").replace("-", "_").replace(".", "_")
        )
        if not app_name.isalnum():
            print(
                "Invalid app name. Please use only alphanumeric characters and underscores."
            )
            return Exit(code=1)

    if app_path is None:
        app_path = Path.cwd()

    print(f"Initializing app {app_name} in {app_path}")

    # create the project directory
    app_path.mkdir(parents=True, exist_ok=True)

    readme_template = jinja2_env.get_template("README.md.jinja2")
    readme_file = app_path.joinpath("README.md")
    readme_file.write_text(readme_template.render(app_name=app_name))

    # create src/{{app_name}} directory
    src_dir = app_path.joinpath("src", app_name)
    src_dir.mkdir(parents=True, exist_ok=True)

    init_template = templates_dir.joinpath("__init__.py")
    shutil.copy(init_template, src_dir.joinpath("__init__.py"))

    gitignore_template = jinja2_env.get_template(".gitignore.jinja2")
    gitignore_file = app_path.joinpath(".gitignore")
    gitignore_file.write_text(gitignore_template.render(app_name=app_name))

    pyproject_toml_template = jinja2_env.get_template("pyproject.toml.jinja2")
    pyproject_file = app_path.joinpath("pyproject.toml")
    pyproject_file.write_text(pyproject_toml_template.render(app_name=app_name))

    # run uv sync in the project directory
    subprocess.run(["uv", "sync"], cwd=app_path)

    # add src/{{app_name}}/api directory
    api_dir = src_dir.joinpath("api")
    api_dir.mkdir(parents=True, exist_ok=True)

    # add src/{{app_name}}/ui directory
    ui_dir = src_dir.joinpath("ui")
    ui_dir.mkdir(parents=True, exist_ok=True)

    # add package.json in the ui directory
    package_json_template = jinja2_env.get_template("package.json.jinja2")
    package_json_file = app_path.joinpath("package.json")
    package_json_file.write_text(package_json_template.render(app_name=app_name))

    # add react and shadcn deps
    subprocess.run(
        [
            "bun",
            "add",
            "react",
            "react-dom",
            "class-variance-authority",
            "clsx",
            "tailwind-merge",
            "lucide-react",
            "tw-animate-css",
            "@tanstack/react-router",
            "@tanstack/react-query",
            "sonner",
        ],
        cwd=app_path,
    )

    # add necessary dev dependencies for the project
    subprocess.run(
        [
            "bun",
            "add",
            "-D",
            "orval",
            "vite",
            "typescript",
            "@types/node",
            "@types/react",
            "@types/react-dom",
            "@vitejs/plugin-react",
            "@tailwindcss/vite",
            "@tanstack/router-plugin",
            "@tanstack/react-router-devtools",
        ],
        cwd=app_path,
    )

    # copy utils.ts to the ui/lib directory
    ui_dir.joinpath("lib").mkdir(parents=True, exist_ok=True)
    utils_template = templates_dir.joinpath("utils.ts")
    shutil.copy(utils_template, ui_dir.joinpath("lib", "utils.ts"))

    # copy globals.css to the ui/styles directory
    globals_css_template = templates_dir.joinpath("globals.css")
    ui_dir.joinpath("styles").mkdir(parents=True, exist_ok=True)
    shutil.copy(globals_css_template, ui_dir.joinpath("styles", "globals.css"))

    # copy components.json to the  directory
    components_json_template = jinja2_env.get_template("components.json.jinja2")
    components_json_file = app_path.joinpath("components.json")
    components_json_file.write_text(components_json_template.render(app_name=app_name))

    # copy tsconfig.json to the ui directory
    tsconfig_json_template = jinja2_env.get_template("tsconfig.json.jinja2")
    tsconfig_json_file = app_path.joinpath("tsconfig.json")
    tsconfig_json_file.write_text(tsconfig_json_template.render(app_name=app_name))

    # copy vite.config.ts to the project directory
    vite_config_ts_template = jinja2_env.get_template("vite.config.ts.jinja2")
    vite_config_ts_file = app_path.joinpath("vite.config.ts")
    vite_config_ts_file.write_text(vite_config_ts_template.render(app_name=app_name))

    # copy index.html to the ui directory
    index_html_template = jinja2_env.get_template("index.html.jinja2")
    index_html_file = ui_dir.joinpath("index.html")
    index_html_file.write_text(index_html_template.render(app_name=app_name))

    # copy main.tsx to the ui directory
    main_tsx_template = templates_dir.joinpath("main.tsx")
    shutil.copy(main_tsx_template, ui_dir.joinpath("main.tsx"))

    # add button component via shadcn cli
    subprocess.run(
        ["bun", "x", "shadcn@latest", "add", "button"],
        cwd=app_path,
    )

    # add ui/routes directory
    routes_dir = ui_dir.joinpath("routes")
    routes_dir.mkdir(parents=True, exist_ok=True)

    # add app_path/src/{{app_name}}/__dist__ directory
    dist_dir = app_path.joinpath("src", app_name, "__dist__")
    dist_dir.mkdir(parents=True, exist_ok=True)

    # add a .gitkeep file to the dist directory
    dist_gitkeep_file = dist_dir.joinpath(".gitkeep")
    dist_gitkeep_file.touch()

    # add ui/routes/index.tsx
    routes_index_tsx_template = templates_dir.joinpath("routes/index.tsx")
    shutil.copy(routes_index_tsx_template, routes_dir.joinpath("index.tsx"))

    # add ui/routes/__root.tsx
    routes_root_tsx_template = templates_dir.joinpath("routes/__root.tsx")
    shutil.copy(routes_root_tsx_template, routes_dir.joinpath("__root.tsx"))

    # copy mode-toggle.tsx to the ui/components directory
    mode_toggle_tsx_template = templates_dir.joinpath("components/mode-toggle.tsx")
    shutil.copy(
        mode_toggle_tsx_template, ui_dir.joinpath("components", "mode-toggle.tsx")
    )

    # copy theme-provider.tsx to the ui/components directory
    theme_provider_tsx_template = templates_dir.joinpath(
        "components/theme-provider.tsx"
    )
    shutil.copy(
        theme_provider_tsx_template, ui_dir.joinpath("components", "theme-provider.tsx")
    )

    # add ui/types directory
    ui_dir.joinpath("types").mkdir(parents=True, exist_ok=True)

    # copy vite-env.d.ts to the ui/types directory
    vite_env_d_ts_template = templates_dir.joinpath("vite-env.d.ts")
    shutil.copy(vite_env_d_ts_template, ui_dir.joinpath("types", "vite-env.d.ts"))

    # copy selector.ts to the ui/lib directory
    selector_ts_template = templates_dir.joinpath("selector.ts")
    shutil.copy(selector_ts_template, ui_dir.joinpath("lib", "selector.ts"))

    # render and copy .cursor/rules/project.mdc to the project directory
    # prepare .cursor/rules directory
    app_path.joinpath(".cursor", "rules").mkdir(parents=True, exist_ok=True)
    project_mdc_template = jinja2_env.get_template(".cursor/rules/project.mdc.jinja2")
    project_mdc_file = app_path.joinpath(".cursor", "rules", "project.mdc")
    project_mdc_file.write_text(project_mdc_template.render(app_name=app_name))

    # copy app.py to the project directory
    app_py_template = jinja2_env.get_template("app.py.jinja2")
    (app_path.joinpath("src", app_name, "api", "app.py")).write_text(
        app_py_template.render(app_name=app_name)
    )

    # run uv run apx openapi {{app_name}}.api.app:app node_modules/.tmp/openapi.json
    subprocess.run(
        [
            "uv",
            "run",
            "apx",
            "openapi",
            f"{app_name}.api.app:app",
            "node_modules/.tmp/openapi.json",
        ],
        cwd=app_path,
    )

    # if git is installed, initialize the project
    if shutil.which("git") is not None:
        subprocess.run(["git", "init"], cwd=app_path)

    # run the build
    subprocess.run(["bun", "run", "build"], cwd=app_path)


@app.command(name="openapi", help="Generate OpenAPI schema from FastAPI app")
def openapi(
    app_name: str = Argument(
        ..., help="App module name in form of some.package.file:app"
    ),
    output_path: Path = Argument(..., help="The path to the output file"),
    version: bool | None = version_option,
):
    # Split the app_name into module path and attribute name (like uvicorn does)
    if ":" not in app_name:
        print(f"Invalid app name format. Expected format: some.package.file:app")
        return Exit(code=1)

    module_path, attribute_name = app_name.split(":", 1)

    # Import the module
    try:
        module = importlib.import_module(module_path)
    except ImportError as e:
        print(f"Failed to import module {module_path}: {e}")
        return Exit(code=1)

    # Get the app attribute from the module
    try:
        app_instance = getattr(module, attribute_name)
    except AttributeError:
        print(f"Module {module_path} does not have attribute '{attribute_name}'")
        return Exit(code=1)

    if not isinstance(app_instance, FastAPI):
        print(f"'{attribute_name}' is not a FastAPI app instance.")
        return Exit(code=1)

    spec = app_instance.openapi()
    output_path.parent.mkdir(parents=True, exist_ok=True)
    output_path.write_text(json.dumps(spec, indent=2))


def entrypoint():
    app()


if __name__ == "__main__":
    entrypoint()
