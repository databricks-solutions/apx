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

```bash
uvx --index https://databricks-solutions.github.io/apx/simple apx init
```

## 📚 Documentation

[Documentation](https://databricks-solutions.github.io/apx/)

## ⬆️ Upgrading `apx`

To upgrade `apx`, you can use the following command:

```bash
uv sync --upgrade-package apx --index https://databricks-solutions.github.io/apx/simple
```

### License and Third Party Libraries

#### License

© 2025 Databricks, Inc. All rights reserved. The source in this project is provided subject to the [Databricks License](LICENSE.md).

---

#### Bundled Bun Runtime

`apx` bundles the [Bun](https://bun.sh) JavaScript runtime directly into its wheels to provide an end-to-end development experience for users. This means users don't need to separately install Node.js, npm, or any JavaScript toolchain — everything required for frontend development is included out of the box.

Bun is licensed under the **MIT License**. See the [Bun License](https://github.com/oven-sh/bun/blob/main/LICENSE.md) for details.

---

#### Rust Crates

The core of `apx` is written in Rust. Below is the complete list of Rust crates used:

| Crate                          | Description                                                        | License              | Source                                                         |
| ------------------------------ | ------------------------------------------------------------------ | -------------------- | -------------------------------------------------------------- |
| clap                           | Command line argument parser with derive macros for CLI definition | MIT/Apache-2.0       | [GitHub](https://github.com/clap-rs/clap)                      |
| dirs                           | Platform-specific standard directories (config, cache, data paths) | MIT/Apache-2.0       | [GitHub](https://github.com/dirs-dev/dirs-rs)                  |
| dialoguer                      | Interactive command-line prompts and user input handling           | MIT                  | [GitHub](https://github.com/console-rs/dialoguer)              |
| indicatif                      | Progress bars, spinners, and CLI status indicators                 | MIT                  | [GitHub](https://github.com/console-rs/indicatif)              |
| rand                           | Random number generation for various use cases                     | MIT/Apache-2.0       | [GitHub](https://github.com/rust-random/rand)                  |
| tera                           | Jinja2-like template engine for project scaffolding                | MIT                  | [GitHub](https://github.com/Keats/tera)                        |
| toml                           | TOML parsing and serialization for configuration files             | MIT/Apache-2.0       | [GitHub](https://github.com/toml-rs/toml)                      |
| toml_edit                      | TOML editing while preserving formatting and comments              | MIT/Apache-2.0       | [GitHub](https://github.com/toml-rs/toml)                      |
| walkdir                        | Recursive directory traversal for file operations                  | MIT/Unlicense        | [GitHub](https://github.com/BurntSushi/walkdir)                |
| chrono                         | Date and time handling with timezone support                       | MIT/Apache-2.0       | [GitHub](https://github.com/chronotope/chrono)                 |
| axum                           | Modern async web framework with WebSocket support for dev server   | MIT                  | [GitHub](https://github.com/tokio-rs/axum)                     |
| reqwest                        | HTTP client for making API requests and downloading resources      | MIT/Apache-2.0       | [GitHub](https://github.com/seanmonstar/reqwest)               |
| serde                          | Serialization/deserialization framework for data structures        | MIT/Apache-2.0       | [GitHub](https://github.com/serde-rs/serde)                    |
| serde_json                     | JSON serialization/deserialization for API communication           | MIT/Apache-2.0       | [GitHub](https://github.com/serde-rs/json)                     |
| serde_yaml                     | YAML serialization/deserialization for config files                | MIT/Apache-2.0       | [GitHub](https://github.com/dtolnay/serde-yaml)                |
| tokio                          | Async runtime powering all concurrent operations                   | MIT                  | [GitHub](https://github.com/tokio-rs/tokio)                    |
| tokio-stream                   | Stream utilities and adapters for async iteration                  | MIT                  | [GitHub](https://github.com/tokio-rs/tokio)                    |
| tokio-util                     | Additional utilities for Tokio (I/O helpers, codecs)               | MIT                  | [GitHub](https://github.com/tokio-rs/tokio)                    |
| futures-util                   | Utilities for working with futures and async streams               | MIT/Apache-2.0       | [GitHub](https://github.com/rust-lang/futures-rs)              |
| sysinfo                        | System information retrieval (processes, CPU, memory)              | MIT                  | [GitHub](https://github.com/GuillaumeGomez/sysinfo)            |
| tracing                        | Application-level tracing and structured logging                   | MIT                  | [GitHub](https://github.com/tokio-rs/tracing)                  |
| tracing-subscriber             | Tracing event subscribers and formatters                           | MIT                  | [GitHub](https://github.com/tokio-rs/tracing)                  |
| opentelemetry                  | Observability SDK for distributed tracing and metrics              | Apache-2.0           | [GitHub](https://github.com/open-telemetry/opentelemetry-rust) |
| opentelemetry_sdk              | OpenTelemetry SDK implementation with Tokio runtime                | Apache-2.0           | [GitHub](https://github.com/open-telemetry/opentelemetry-rust) |
| opentelemetry-otlp             | OTLP exporter for sending telemetry to collectors                  | Apache-2.0           | [GitHub](https://github.com/open-telemetry/opentelemetry-rust) |
| opentelemetry-appender-tracing | Bridge between tracing and OpenTelemetry                           | Apache-2.0           | [GitHub](https://github.com/open-telemetry/opentelemetry-rust) |
| opentelemetry-proto            | OpenTelemetry protocol buffer definitions                          | Apache-2.0           | [GitHub](https://github.com/open-telemetry/opentelemetry-rust) |
| notify                         | Cross-platform file system change notifications for hot reload     | CC0-1.0/Artistic-2.0 | [GitHub](https://github.com/notify-rs/notify)                  |
| tokio-tungstenite              | WebSocket client/server for real-time communication                | MIT                  | [GitHub](https://github.com/snapview/tokio-tungstenite)        |
| tokio-postgres                 | Async PostgreSQL client for database operations                    | MIT/Apache-2.0       | [GitHub](https://github.com/sfackler/rust-postgres)            |
| schemars                       | JSON Schema generation from Rust types for API docs                | MIT                  | [GitHub](https://github.com/GREsau/schemars)                   |
| biome_css_parser               | CSS parser for analyzing and transforming stylesheets              | MIT                  | [GitHub](https://github.com/biomejs/biome)                     |
| biome_css_syntax               | CSS syntax tree definitions for parsing                            | MIT                  | [GitHub](https://github.com/biomejs/biome)                     |
| biome_rowan                    | Red-green tree library for syntax analysis                         | MIT                  | [GitHub](https://github.com/biomejs/biome)                     |
| url                            | URL parsing and manipulation                                       | MIT/Apache-2.0       | [GitHub](https://github.com/servo/rust-url)                    |
| lancedb                        | Vector database with search capabilities (used in mcp search)      | Apache-2.0           | [GitHub](https://github.com/lancedb/lancedb)                   |
| arrow                          | Apache Arrow columnar data format implementation                   | Apache-2.0           | [GitHub](https://github.com/apache/arrow-rs)                   |
| lance-index                    | Lance index implementation for vector search                       | Apache-2.0           | [GitHub](https://github.com/lancedb/lance)                     |
| zip                            | ZIP archive reading/writing for packaging                          | MIT                  | [GitHub](https://github.com/zip-rs/zip2)                       |
| rayon                          | Data parallelism library for parallel iteration                    | MIT/Apache-2.0       | [GitHub](https://github.com/rayon-rs/rayon)                    |
| hex                            | Hexadecimal encoding/decoding utilities                            | MIT/Apache-2.0       | [GitHub](https://github.com/KokaKiwi/rust-hex)                 |
| rusqlite                       | SQLite bindings for local data storage                             | MIT                  | [GitHub](https://github.com/rusqlite/rusqlite)                 |
| prost                          | Protocol Buffers implementation for Rust                           | Apache-2.0           | [GitHub](https://github.com/tokio-rs/prost)                    |
| tempfile                       | Temporary file and directory creation (dev dependency)             | MIT/Apache-2.0       | [GitHub](https://github.com/Stebalien/tempfile)                |

---

#### Python Libraries

Libraries used in generated projects and the Python runtime:

| Library                   | Description                                                     | License      | Source                                                      |
| ------------------------- | --------------------------------------------------------------- | ------------ | ----------------------------------------------------------- |
| FastAPI                   | High-performance API framework based on Starlette               | MIT          | [GitHub](https://github.com/tiangolo/fastapi)               |
| Pydantic                  | Data validation and settings management using Python type hints | MIT          | [GitHub](https://github.com/pydantic/pydantic)              |
| SQLModel                  | SQLAlchemy-like ORM for Python                                  | MIT          | [GitHub](https://github.com/fastapi/sqlmodel)               |
| Databricks SDK for Python | Official Databricks SDK for Python                              | Apache-2.0   | [GitHub](https://github.com/databricks/databricks-sdk-py)   |
| uv                        | Fast, modern Python package manager                             | MIT          | [GitHub](https://github.com/astral-sh/uv)                   |
| uvicorn                   | ASGI server for Python                                          | BSD-3-Clause | [GitHub](https://github.com/encode/uvicorn)                 |
| httpx                     | HTTP client for Python                                          | BSD-3-Clause | [GitHub](https://github.com/encode/httpx)                   |
| watchfiles                | File change monitoring for Python                               | MIT          | [GitHub](https://github.com/samuelcolvin/watchfiles)        |
| hatchling                 | Build backend for Python                                        | MIT          | [GitHub](https://github.com/pypa/hatch)                     |
| uv-dynamic-versioning     | Dynamic versioning for Python packages                          | MIT          | [GitHub](https://github.com/ninoseki/uv-dynamic-versioning) |

---

#### JavaScript/TypeScript Libraries

Libraries used in generated frontend projects:

| Library      | Description                                          | License    | Source                                                |
| ------------ | ---------------------------------------------------- | ---------- | ----------------------------------------------------- |
| React        | Library for building user interfaces                 | MIT        | [GitHub](https://github.com/facebook/react)           |
| TypeScript   | Typed programming language for web development       | Apache-2.0 | [GitHub](https://github.com/microsoft/TypeScript)     |
| shadcn/ui    | UI component library for React                       | MIT        | [GitHub](https://github.com/shadcn/ui)                |
| Vite         | Frontend build tool and dev server                   | MIT        | [GitHub](https://github.com/vitejs/vite)              |
| Tailwind CSS | Utility-first CSS framework for rapid UI development | MIT        | [GitHub](https://github.com/tailwindlabs/tailwindcss) |
| jinja2       | Template engine for Python (used in scaffolding)     | MIT        | [GitHub](https://github.com/pallets/jinja)            |
| rich         | CLI interface library for Python                     | MIT        | [GitHub](https://github.com/Textualize/rich)          |
| typer        | Library for building CLI applications                | MIT        | [GitHub](https://github.com/fastapi/typer)            |
