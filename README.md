<p align="center">
    <a href="https://github.com/databricks-solutions/apx">
        <img src="https://raw.githubusercontent.com/databricks-solutions/apx/refs/heads/main/assets/logo.svg" class="align-center" width="200" height="200" alt="logo" />
    </a>
</p>

<p align="center">
    <b>🚀 <code>apx</code> is the toolkit for building Databricks Apps ⚡</b>
</p>

<div align="center">

![Databricks](https://img.shields.io/badge/databricks-000000?logo=databricks&logoColor=red)
![FastAPI](https://img.shields.io/badge/FastAPI-109989?logo=fastapi&logoColor=white)
![Pydantic](https://img.shields.io/badge/Pydantic-E92063?logo=pydantic&logoColor=white)
![uv](https://img.shields.io/badge/uv-000000?logo=uv&logoColor=white)
![React](https://img.shields.io/badge/React-20232A?logo=react&logoColor=61DAFB)
![TypeScript](https://img.shields.io/badge/TypeScript-3178C6?logo=typescript&logoColor=white)
![Bun](https://img.shields.io/badge/Bun-000000?logo=bun&logoColor=white)
![shadcn/ui](https://img.shields.io/badge/shadcn%2Fui-000000?logo=shadcnui&logoColor=white)

</div>

---

✨ `apx` bundles together a set of tools and libraries to help you with app development lifecycle: develop, build and deploy.

💡 The main idea of `apx` is to provide convenient, fast and AI-friendly development experience.

## 🚀 Quickstart

1. Run the command to initialize the project:

```bash
uvx git+https://github.com/databricks-solutions/apx.git init
```

2. Open the project in Cursor (or any IDE of your choice, for that matter), allow usage of apx and shadcn mcp
3. Start providing prompts to build an app, e.g.:

```text
Use apx mcp server to start development server, then create a nice application for order management.
Use shadcn mcp to add new components, make sure style is consistent and data is properly fetched from the backend. Start with mocking the backend data (yet use pydantic models), then implement the actual backend.
```

4. Deploy whenever ready via Databricks CLI:

```bash
databricks bundle deploy -p <your-profile>
```

## 🛠️ Stack

`apx` is built on top of the following stack:

- 🐍 Python + FastAPI in the backend
- ⚛️ React + shadcn/ui in the frontend

🔌 To connect the frontend and the backend, `apx` uses `orval` to generate the client code from the OpenAPI schema.

## 🚀 Init

To kickstart your app, please make sure you have:

- ✅ `uv` installed
- ✅ `bun` installed
- ✅ `git` installed

Then you can use the following command:

```bash
uvx git+https://github.com/databricks-solutions/apx.git init
```

This will launch an interactive prompt that will guide you through:

- 📝 Naming your app (or using a randomly generated name)
- 🔧 Selecting a Databricks profile (if you have any configured)
- 🤖 Setting up AI assistant rules (cursor/vscode/codex/claude)

The app will be created in the current working directory by default. You can specify a different path via:

```bash
uvx git+https://github.com/databricks-solutions/apx.git init /path/to/your/app
```

### ⚙️ Non-Interactive Mode

You can also specify all options via command-line flags to skip the prompts:

```bash
uvx https://github.com/databricks-solutions/apx.git init \
  --name my-app \
  --template essential \
  --profile my-profile \
  --assistant cursor \
  my-app
```

This will create a new app in the `my-app` directory with the app name `my-app`.

## 📁 Project Structure

The project structure is as follows:

```
my-app
├── package.json
├── pyproject.toml
├── README.md
├── src
│   └── sample
│       ├── __dist__
│       ├── backend
│       │   ├── app.py
│       │   ├── config.py
│       │   ├── dependencies.py
│       │   ├── models.py
│       │   ├── router.py
│       │   ├── runtime.py
│       │   └── utils.py
│       └── ui
│           ├── components
│           ├── lib
│           ├── routes
│           ├── main.tsx
```

📦 The `__dist__` directory is the directory where the frontend bundle is stored, so it can be served by the backend.

## 📚 Databricks SDK Documentation

The Databricks SDK documentation is available via MCP server. You can search for methods and models using the following command:

```bash
uv run apx mcp sdk search <query>
```

This will return a list of methods and models that match the query.

## ⬆️ Upgrading `apx`

To upgrade `apx`, you can use the following command:

```bash
uv sync --upgrade-package apx
```

This will pull the latest changes from the main branch and update the `apx` package.

## 🎮 Commands

### 🚀 `init`

```bash
uvx git+https://github.com/databricks-solutions/apx.git init
```

Initializes a new app project with interactive prompts for configuration. Supports optional flags to skip prompts:

- `--name, -n`: Specify the app name
- `--template, -t`: Choose a template (essential/stateful)
  - 🎯 Essential template is a basic template with UI and API.
  - 💾 Stateful template also includes Lakebase integration via `sqlmodel`.
- `--profile, -p`: Specify a Databricks profile
- `--assistant, -a`: Choose AI assistant rules (cursor/vscode/codex/claude)
- `--layout, -l`: Choose the layout (basic/sidebar)
- `--skip-frontend-dependencies`: Skip installing frontend dependencies (bun packages)
- `--skip-backend-dependencies`: Skip installing backend dependencies (uv sync)
- `--skip-build`: Skip building the project after initialization

### 🔥 `dev` - Development Server Management

The `dev` command group manages development servers in detached mode:

#### Start Development Servers

```bash
uv run apx dev start
```

Starts backend, frontend, and OpenAPI watcher in detached mode.

Options:

- `--host`: Host for dev, frontend, and backend servers (default: localhost)
- `--api-prefix`: URL prefix for API routes (default: /api)
- `--obo/--no-obo`: Enable/disable On-Behalf-Of header (default: enabled)
- `--openapi/--no-openapi`: Enable/disable OpenAPI watcher (default: enabled)
- `--max-retries`: Maximum number of retry attempts for processes (default: 10)
- `--watch, -w`: Start servers and tail logs until Ctrl+C, then stop all servers

#### Check Server Status

```bash
uv run apx dev status
```

Shows status of all running development servers (backend, frontend, OpenAPI watcher).

#### View Logs

```bash
uv run apx dev logs
```

Displays historical logs from development servers.

Options:

- `--duration, -d`: Show logs from last N seconds
- `--follow, -f`: Follow log output (like tail -f). Streams new logs continuously.
- `--ui`: Show only frontend logs
- `--backend`: Show only backend logs
- `--openapi`: Show only OpenAPI logs
- `--app`: Show only application logs (from your app code)
- `--system`: Show only system logs from the apx dev server
- `--raw`: Show raw log output without prefix formatting

#### Restart Development Servers

```bash
uv run apx dev restart
```

Restarts all running development servers, attempting to reuse the same ports.

#### Stop Development Servers

```bash
uv run apx dev stop
```

Stops all running development servers.

#### Check Project Code

```bash
uv run apx dev check
```

Checks the project code for errors using TypeScript compiler and basedpyright.

#### MCP Server

```bash
uv run apx dev mcp
```

Starts MCP server that provides tools for managing development servers. The MCP server runs over stdio and provides tools for start, restart, stop, status, and get_metadata operations.

#### Apply Addon

```bash
uv run apx dev apply
```

Applies an addon to an existing project.

### 📦 `build`

```bash
uv run apx build
```

Prepares the app for deployment by building both frontend assets and Python wheel.

Options:

- `--build-path`: Path to the build directory where artifacts will be placed, relative to the app path (default: .build)
- `--skip-ui-build`: Skip the UI build step

### 🔧 `openapi`

```bash
uv run apx openapi
```

Manually generates OpenAPI schema and orval client.

Options:

- `--watch, -w`: Watch for changes and regenerate automatically
- `--force, -f`: Force regeneration even if schema hasn't changed

Note: you don't need to run this command manually, the watcher will run automatically when you start the development server.

### 📝 `shadcn` directory repositories

When the project is initialized, the following directories are added to the `shadcn` directory (via `repositories` key in the `components.json` file):

- https://animate-ui.com/ - for animations. MIT License.
- https://ai-sdk.dev/ - for AI components (e.g. chat and prompts). Apache-2.0 License.
- https://svgl.app/docs/shadcn-ui - various SVG icons. MIT License.

We've carefully selected these repositories to ensure that the components are of high quality and are well-maintained.

If you don't want to use these repositories, you can remove them from the `components.json` file.

## 📜 Project todos

- [x] MCP of apx commands
- [ ] Add chat template
- [ ] Add a way to add a custom template

### License and Third Party Libraries

#### License

© 2025 Databricks, Inc. All rights reserved. The source in this project is provided subject to the [Databricks License](LICENSE.md).

#### Third Party Libraries

| library                   | description                                                                                                                            | license      | source                                                      |
| ------------------------- | -------------------------------------------------------------------------------------------------------------------------------------- | ------------ | ----------------------------------------------------------- |
| FastAPI                   | High-performance API framework based on Starlette                                                                                      | MIT          | [GitHub](https://github.com/tiangolo/fastapi)               |
| Pydantic                  | Data validation and settings management using Python type hints                                                                        | MIT          | [GitHub](https://github.com/pydantic/pydantic)              |
| SQLModel                  | SQLAlchemy-like ORM for Python                                                                                                         | MIT          | [GitHub](https://github.com/fastapi/sqlmodel)               |
| Databricks SDK for Python | Official Databricks SDK for Python                                                                                                     | Apache-2.0   | [GitHub](https://github.com/databricks/databricks-sdk-py)   |
| orval                     | OpenAPI client generator                                                                                                               | MIT          | [GitHub](https://github.com/orval-labs/orval)               |
| shadcn/ui                 | UI library for React                                                                                                                   | MIT          | [GitHub](https://github.com/shadcn/ui)                      |
| React                     | Library for building user interfaces                                                                                                   | MIT          | [GitHub](https://github.com/facebook/react)                 |
| TypeScript                | Programming language for web development                                                                                               | Apache-2.0   | [GitHub](https://github.com/microsoft/TypeScript)           |
| Bun                       | JavaScript runtime                                                                                                                     | MIT          | [GitHub](https://github.com/oven-sh/bun)                    |
| uv                        | Fast, modern Python package manager                                                                                                    | MIT          | [GitHub](https://github.com/astral-sh/uv)                   |
| jinja2                    | Template engine for Python                                                                                                             | MIT          | [GitHub](https://github.com/pallets/jinja)                  |
| rich                      | CLI interface library for Python                                                                                                       | MIT          | [GitHub](https://github.com/Textualize/rich)                |
| typer                     | Typer is a library for building CLI applications                                                                                       | MIT          | [GitHub](https://github.com/fastapi/typer)                  |
| uvicorn                   | ASGI server for Python                                                                                                                 | BSD-3-Clause | [GitHub](https://github.com/encode/uvicorn)                 |
| httpx                     | HTTP client for Python                                                                                                                 | BSD-3-Clause | [GitHub](https://github.com/encode/httpx)                   |
| watchfiles                | File change monitoring for Python                                                                                                      | MIT          | [GitHub](https://github.com/samuelcolvin/watchfiles)        |
| hatchling                 | Build backend for Python                                                                                                               | MIT          | [GitHub](https://github.com/pypa/hatch)                     |
| uv-dynamic-versioning     | Dynamic versioning for Python packages                                                                                                 | MIT          | [GitHub](https://github.com/ninoseki/uv-dynamic-versioning) |
| vite                      | Frontend build tool for JavaScript                                                                                                     | MIT          | [GitHub](https://github.com/vitejs/vite)                    |
| tailwindcss               | Utility-first CSS framework for rapid UI development                                                                                   | MIT          | [GitHub](https://github.com/tailwindlabs/tailwindcss)       |
| smol-toml                 | Tom's Obvious, Minimal Language for JS                                                                                                 | MIT          | [GitHub](https://github.com/squirrelchat/smol-toml)         |
| psutil                    | Cross-platform library for retrieving information on running processes and system utilization (CPU, memory, disks, network) in Python. | BSD-3-Clause | [GitHub](https://github.com/giampaolo/psutil)               |
