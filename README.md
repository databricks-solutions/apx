<p align="center">
    <a href="https://github.com/renardeinside/apx">
        <img src="https://raw.githubusercontent.com/renardeinside/apx/refs/heads/main/assets/logo.svg" class="align-center" width="200" height="200" alt="logo" />
    </a>
</p>

<p align="center">
    <b>🚀 <code>apx</code> is the toolkit for building Databricks Apps ⚡</b>
</p>

![Databricks](https://img.shields.io/badge/databricks-000000?logo=databricks&logoColor=red)
![FastAPI](https://img.shields.io/badge/FastAPI-109989?logo=fastapi&logoColor=white)
![Pydantic](https://img.shields.io/badge/Pydantic-E92063?logo=pydantic&logoColor=white)
![uv](https://img.shields.io/badge/uv-000000?logo=uv&logoColor=white)
![React](https://img.shields.io/badge/React-20232A?logo=react&logoColor=61DAFB)
![TypeScript](https://img.shields.io/badge/TypeScript-3178C6?logo=typescript&logoColor=white)
![Bun](https://img.shields.io/badge/Bun-000000?logo=bun&logoColor=white)
![shadcn/ui](https://img.shields.io/badge/shadcn%2Fui-000000?logo=shadcnui&logoColor=white)

---

✨ `apx` bundles together a set of tools and libraries to help you build your app, as well as comes with convenience utilities for building your app.

💡 The main idea of `apx` is to provide convenient, fast and AI-friendly development experience.

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
uvx git+https://github.com/renardeinside/apx.git init
```

This will launch an interactive prompt that will guide you through:

- 📝 Naming your app (or using a randomly generated name)
- 🔧 Selecting a Databricks profile (if you have any configured)
- 🤖 Setting up AI assistant rules (cursor/vscode/codex/claude)

The app will be created in the current working directory by default.

### ⚙️ Non-Interactive Mode

You can also specify all options via command-line flags to skip the prompts:

```bash
uvx git+https://github.com/renardeinside/apx.git init \
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

## 🎮 Commands

### 🚀 `init`

```bash
uvx git+https://github.com/renardeinside/apx.git init
```

Initializes a new app project with interactive prompts for configuration. Supports optional flags to skip prompts:

- `--name, -n`: Specify the app name
- `--template, -t`: Choose a template (essential/stateful)
  - 🎯 Essential template is a basic template with UI and API.
  - 💾 Stateful template also includes Lakebase integration via `sqlmodel`.
- `--profile, -p`: Specify a Databricks profile
- `--assistant, -a`: Choose AI assistant rules (cursor/vscode/codex/claude)
- `--layout, -l`: Choose the layout (basic/sidebar)

### 🔥 `dev`

```bash
uv run apx dev
```

Launches both backend and frontend development servers with hot reload.

### 📦 `build`

```bash
uv run apx build
```

Prepares the app for deployment by building both frontend assets and Python wheel.
