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

Install `apx`:

```bash
# macOS/Linux
curl -fsSL https://databricks-solutions.github.io/apx/install.sh | sh

# Windows
irm https://databricks-solutions.github.io/apx/install.ps1 | iex
```

Initialize a new project:

```bash
apx init
```

## 📚 Documentation

[Documentation](https://databricks-solutions.github.io/apx/)

## ⬆️ Upgrading `apx`

```bash
apx upgrade
```

### License and Third Party Libraries

#### License

© 2025 Databricks, Inc. All rights reserved. The source in this project is provided subject to the [Databricks License](LICENSE.md).

---

#### Rust Crates

The core of `apx` is written in Rust. Below is the complete list of Rust crates used:

| Crate                          | Description                                                        | License              | Source                                                         |
| ------------------------------ | ------------------------------------------------------------------ | -------------------- | -------------------------------------------------------------- |
| axum                           | Modern async web framework with WebSocket support for dev server   | MIT                  | [GitHub](https://github.com/tokio-rs/axum)                     |
| biome_css_parser               | CSS parser for analyzing and transforming stylesheets              | MIT                  | [GitHub](https://github.com/biomejs/biome)                     |
| biome_css_syntax               | CSS syntax tree definitions for parsing                            | MIT                  | [GitHub](https://github.com/biomejs/biome)                     |
| biome_rowan                    | Red-green tree library for syntax analysis                         | MIT                  | [GitHub](https://github.com/biomejs/biome)                     |
| chrono                         | Date and time handling with timezone support                       | MIT/Apache-2.0       | [GitHub](https://github.com/chronotope/chrono)                 |
| clap                           | Command line argument parser with derive macros for CLI definition | MIT/Apache-2.0       | [GitHub](https://github.com/clap-rs/clap)                      |
| console                        | Terminal styling and interaction utilities                         | MIT                  | [GitHub](https://github.com/console-rs/console)                |
| dialoguer                      | Interactive command-line prompts and user input handling           | MIT                  | [GitHub](https://github.com/console-rs/dialoguer)              |
| dirs                           | Platform-specific standard directories (config, cache, data paths) | MIT/Apache-2.0       | [GitHub](https://github.com/dirs-dev/dirs-rs)                  |
| flate2                         | DEFLATE compression and decompression                              | MIT/Apache-2.0       | [GitHub](https://github.com/rust-lang/flate2-rs)               |
| futures-util                   | Utilities for working with futures and async streams               | MIT/Apache-2.0       | [GitHub](https://github.com/rust-lang/futures-rs)              |
| hex                            | Hexadecimal encoding/decoding utilities                            | MIT/Apache-2.0       | [GitHub](https://github.com/KokaKiwi/rust-hex)                 |
| indicatif                      | Progress bars, spinners, and CLI status indicators                 | MIT                  | [GitHub](https://github.com/console-rs/indicatif)              |
| notify                         | Cross-platform file system change notifications for hot reload     | CC0-1.0/Artistic-2.0 | [GitHub](https://github.com/notify-rs/notify)                  |
| opentelemetry                  | Observability SDK for distributed tracing and metrics              | Apache-2.0           | [GitHub](https://github.com/open-telemetry/opentelemetry-rust) |
| opentelemetry_sdk              | OpenTelemetry SDK implementation with Tokio runtime                | Apache-2.0           | [GitHub](https://github.com/open-telemetry/opentelemetry-rust) |
| opentelemetry-otlp             | OTLP exporter for sending telemetry to collectors                  | Apache-2.0           | [GitHub](https://github.com/open-telemetry/opentelemetry-rust) |
| opentelemetry-appender-tracing | Bridge between tracing and OpenTelemetry                           | Apache-2.0           | [GitHub](https://github.com/open-telemetry/opentelemetry-rust) |
| opentelemetry-proto            | OpenTelemetry protocol buffer definitions                          | Apache-2.0           | [GitHub](https://github.com/open-telemetry/opentelemetry-rust) |
| prost                          | Protocol Buffers implementation for Rust                           | Apache-2.0           | [GitHub](https://github.com/tokio-rs/prost)                    |
| rand                           | Random number generation for various use cases                     | MIT/Apache-2.0       | [GitHub](https://github.com/rust-random/rand)                  |
| rayon                          | Data parallelism library for parallel iteration                    | MIT/Apache-2.0       | [GitHub](https://github.com/rayon-rs/rayon)                    |
| reqwest                        | HTTP client for making API requests and downloading resources      | MIT/Apache-2.0       | [GitHub](https://github.com/seanmonstar/reqwest)               |
| rmcp                           | Rust SDK for the Model Context Protocol                            | MIT/Apache-2.0       | [Crates.io](https://crates.io/crates/rmcp)                     |
| ruff_python_ast                | Python AST definitions from the Ruff project                       | MIT                  | [GitHub](https://github.com/astral-sh/ruff)                    |
| ruff_python_parser             | Python parser from Ruff for AST manipulation                       | MIT                  | [GitHub](https://github.com/astral-sh/ruff)                    |
| ruff_text_size                 | Text size utilities from the Ruff project                          | MIT/Apache-2.0       | [GitHub](https://github.com/astral-sh/ruff)                    |
| rust-embed                     | Embed files into Rust binary at compile time                       | MIT                  | [GitHub](https://github.com/pyrossh/rust-embed)                |
| schemars                       | JSON Schema generation from Rust types for API docs                | MIT                  | [GitHub](https://github.com/GREsau/schemars)                   |
| serde                          | Serialization/deserialization framework for data structures        | MIT/Apache-2.0       | [GitHub](https://github.com/serde-rs/serde)                    |
| serde_json                     | JSON serialization/deserialization for API communication           | MIT/Apache-2.0       | [GitHub](https://github.com/serde-rs/json)                     |
| serde_with                     | Custom serde serialization helpers                                 | MIT/Apache-2.0       | [GitHub](https://github.com/jonasbb/serde_with)                |
| serde_yaml                     | YAML serialization/deserialization for config files                | MIT/Apache-2.0       | [GitHub](https://github.com/dtolnay/serde-yaml)                |
| sha2                           | SHA-2 hash functions for integrity verification                    | MIT/Apache-2.0       | [GitHub](https://github.com/RustCrypto/hashes)                 |
| similar                        | Text diffing library for addon apply diffs                         | MIT                  | [GitHub](https://github.com/mitsuhiko/similar)                 |
| sqlx                           | Async SQL toolkit with compile-time checked queries (SQLite)       | MIT/Apache-2.0       | [GitHub](https://github.com/launchbadge/sqlx)                  |
| swc_atoms                      | Interned string atoms for the SWC compiler                         | Apache-2.0           | [GitHub](https://github.com/swc-project/swc)                   |
| swc_common                     | Common utilities for SWC TypeScript/JavaScript AST                 | Apache-2.0           | [GitHub](https://github.com/swc-project/swc)                   |
| swc_ecma_ast                   | ECMAScript AST definitions for TypeScript parsing                  | Apache-2.0           | [GitHub](https://github.com/swc-project/swc)                   |
| swc_ecma_codegen               | ECMAScript code generation from AST                                | Apache-2.0           | [GitHub](https://github.com/swc-project/swc)                   |
| sysinfo                        | System information retrieval (processes, CPU, memory)              | MIT                  | [GitHub](https://github.com/GuillaumeGomez/sysinfo)            |
| tar                            | TAR archive reading and writing                                    | MIT/Apache-2.0       | [GitHub](https://github.com/alexcrichton/tar-rs)               |
| tauri                          | Desktop application framework (used for Studio)                    | MIT/Apache-2.0       | [GitHub](https://github.com/tauri-apps/tauri)                  |
| tempfile                       | Temporary file and directory creation (dev dependency)             | MIT/Apache-2.0       | [GitHub](https://github.com/Stebalien/tempfile)                |
| tera                           | Jinja2-like template engine for project scaffolding                | MIT                  | [GitHub](https://github.com/Keats/tera)                        |
| thiserror                      | Derive macro for Error trait implementations                       | MIT/Apache-2.0       | [GitHub](https://github.com/dtolnay/thiserror)                 |
| tokio                          | Async runtime powering all concurrent operations                   | MIT                  | [GitHub](https://github.com/tokio-rs/tokio)                    |
| tokio-postgres                 | Async PostgreSQL client for database operations                    | MIT/Apache-2.0       | [GitHub](https://github.com/sfackler/rust-postgres)            |
| tokio-stream                   | Stream utilities and adapters for async iteration                  | MIT                  | [GitHub](https://github.com/tokio-rs/tokio)                    |
| tokio-tungstenite              | WebSocket client/server for real-time communication                | MIT                  | [GitHub](https://github.com/snapview/tokio-tungstenite)        |
| tokio-util                     | Additional utilities for Tokio (I/O helpers, codecs)               | MIT                  | [GitHub](https://github.com/tokio-rs/tokio)                    |
| toml                           | TOML parsing and serialization for configuration files             | MIT/Apache-2.0       | [GitHub](https://github.com/toml-rs/toml)                      |
| toml_edit                      | TOML editing while preserving formatting and comments              | MIT/Apache-2.0       | [GitHub](https://github.com/toml-rs/toml)                      |
| tracing                        | Application-level tracing and structured logging                   | MIT                  | [GitHub](https://github.com/tokio-rs/tracing)                  |
| tracing-subscriber             | Tracing event subscribers and formatters                           | MIT                  | [GitHub](https://github.com/tokio-rs/tracing)                  |
| url                            | URL parsing and manipulation                                       | MIT/Apache-2.0       | [GitHub](https://github.com/servo/rust-url)                    |
| walkdir                        | Recursive directory traversal for file operations                  | MIT/Unlicense        | [GitHub](https://github.com/BurntSushi/walkdir)                |
| which                          | Cross-platform executable path discovery                           | MIT                  | [GitHub](https://github.com/harryfei/which-rs)                 |
| zip                            | ZIP archive reading/writing for packaging (zip2)                   | MIT                  | [GitHub](https://github.com/zip-rs/zip2)                       |

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
