pub const APX_INFO_CONTENT: &str = r#"
This project uses apx toolkit to build a Databricks app.
apx bundles together a set of tools and libraries to help you with the complete app development lifecycle: develop, build and deploy.

## Technology Stack

- **Backend**: Python + FastAPI + Pydantic
- **Frontend**: React + TypeScript + shadcn/ui
- **Build Tools**: uv (Python), bun (JavaScript/TypeScript)

## Tool Usage

All project-scoped tools require an `app_path` parameter — the absolute path to the project directory.
Global tools (like `docs`) do not require `app_path`.
"#;
