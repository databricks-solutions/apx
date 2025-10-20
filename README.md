<p align="center">
    <a href="https://github.com/renardeinside/apx">
        <img src="https://raw.githubusercontent.com/renardeinside/apx/refs/heads/main/assets/logo.svg" class="align-center" width="200" height="200" alt="logo" />
    </a>
</p>

<p align="center">
    <b><code>apx</code> is the toolkit for building Databricks Apps</b>
</p>

---

`apx` bundles together a set of tools and libraries to help you build your app, as well as comes with convenience utilities for building your app.

The main idea of `apx` is to provide convenient, fast and AI-friendly development experience.

## Stack

`apx` is built on top of the following stack:

- Python + FastAPI in the backend
- React + shadcn/ui in the frontend

To connect the frontend and the backend, `apx` uses `orval` to generate the client code from the OpenAPI schema.

## Init

To kickstart your app, please make sure you have:

- `uv` installed
- `bun` installed
- `git` installed

Then you can use the following command:

```bash
uvx git+https://github.com/renardeinside/apx.git init
```

This will create a new app in the current working directory with random app name.

You can also specify the app name and path:

```bash
uvx git+https://github.com/renardeinside/apx.git init my-app my-app
```

This will create a new app in the `my-app` directory with the app name `my-app`.

## Project Structure

The project structure is as follows:

```
app
├── package.json
├── pyproject.toml
├── README.md
├── src
│   └── sample
│       ├── __dist__
│       ├── backend
│       │   ├── app.py
│       │   ├── config.py
│       │   ├── dependencies.py
│       │   ├── models.py
│       │   ├── router.py
│       │   ├── runtime.py
│       │   └── utils.py
│       └── ui
│           ├── components
│           ├── lib
│           ├── routes
│           ├── main.tsx
```

The `__dist__` directory is the directory where the frontend bundle is stored, so it can be served by the backend.

## Commands

```
init -> initializes the app project
dev -> launches back/frontend dev servers
build -> prepares the app for deployment
```
