from rich import print
from typer import Typer

from apx._version import version as apx_version
from apx.cli.build import build as build_command
from apx.cli.dev.commands import dev_app
from apx.cli.init import init as init_command
from apx.cli.openapi import openapi as openapi_command
from apx.cli.version import with_version


app = Typer(
    name="apx | Databricks App Toolkit",
)


@app.callback()
@with_version
def main():
    """Project quickstarter CLI."""
    pass


@app.command(name="version", help="Show the version of apx")
def version():
    print(f"apx version: {apx_version}")


app.command(name="init", help="Initialize a new project")(init_command)

app.command(name="build", help="Build the project (frontend + Python wheel)")(
    build_command
)

app.command(name="openapi", help="Generate OpenAPI schema and orval client")(
    openapi_command
)

# Add dev command group
app.add_typer(dev_app, name="dev")


def entrypoint():
    app()


if __name__ == "__main__":
    entrypoint()
