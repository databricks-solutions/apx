# apx - Application Toolkit for Databricks Apps

`apx` is a toolkit for building Databricks apps. It bundles together a bunch of tools and libraries to help you build your app, as well as comes with convenience utilities for building your app.

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
