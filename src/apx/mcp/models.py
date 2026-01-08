"""Pydantic and SQLModel models for Databricks SDK documentation MCP tools."""

from pydantic import BaseModel, Field
from sqlmodel import Field as SQLField
from sqlmodel import SQLModel


# ============================================================================
# Response Models (Pydantic) - Used for MCP tool responses
# ============================================================================


class SDKParameterInfo(BaseModel):
    """Information about a single parameter in an SDK method."""

    name: str = Field(description="Parameter name")
    annotation: str | None = Field(
        default=None, description="Type annotation as string"
    )
    default: str | None = Field(default=None, description="Default value as string")
    kind: str = Field(
        description="Parameter kind (POSITIONAL_ONLY, POSITIONAL_OR_KEYWORD, etc.)"
    )


class SDKMethodSpec(BaseModel):
    """Specification for a method in the Databricks SDK."""

    service_name: str = Field(description="Service name, e.g., 'clusters'")
    class_name: str = Field(description="API class name, e.g., 'ClustersAPI'")
    method_name: str = Field(description="Method name, e.g., 'create'")
    full_name: str = Field(description="Full name, e.g., 'clusters.create'")
    signature: str = Field(description="Full method signature")
    docstring: str | None = Field(default=None, description="Method documentation")
    rst_docs: str | None = Field(
        default=None, description="RST documentation from docs/ folder"
    )
    has_rst: bool = Field(
        default=False, description="Whether RST documentation is available"
    )
    parameters: list[SDKParameterInfo] = Field(
        default_factory=list, description="List of parameter specifications"
    )


class SDKModelField(BaseModel):
    """Information about a single field in an SDK dataclass/model."""

    name: str = Field(description="Field name")
    type_annotation: str = Field(description="Type annotation as string")
    default: str | None = Field(default=None, description="Default value as string")


class SDKModelSpec(BaseModel):
    """Specification for a dataclass/model in the Databricks SDK."""

    module_name: str = Field(description="Module name, e.g., 'jobs'")
    class_name: str = Field(description="Class name, e.g., 'JobSettings'")
    full_name: str = Field(description="Full name, e.g., 'jobs.JobSettings'")
    docstring: str | None = Field(default=None, description="Class documentation")
    fields: list[SDKModelField] = Field(
        default_factory=list, description="List of field specifications"
    )


class SDKSearchResult(BaseModel):
    """Result of searching the Databricks SDK documentation."""

    methods: list[SDKMethodSpec] = Field(
        default_factory=list, description="Matching methods"
    )
    models: list[SDKModelSpec] = Field(
        default_factory=list, description="Matching models"
    )
    total_methods_indexed: int = Field(
        description="Total number of methods in the index"
    )
    total_models_indexed: int = Field(description="Total number of models in the index")
    rst_coverage: float = Field(
        default=0.0, description="Percentage of methods with RST documentation"
    )


class SDKMethodSpecResponse(BaseModel):
    """Response for get_method_spec tool."""

    method: SDKMethodSpec | None = Field(
        default=None, description="Method specification if found"
    )
    error: str | None = Field(default=None, description="Error message if not found")


class SDKModelSpecResponse(BaseModel):
    """Response for get_model_spec tool."""

    model: SDKModelSpec | None = Field(
        default=None, description="Model specification if found"
    )
    error: str | None = Field(default=None, description="Error message if not found")


class SDKUsageInstructions(BaseModel):
    """Usage instructions for the Databricks SDK."""

    pagination_guide: str = Field(description="Guide for handling paginated responses")
    long_running_operations_guide: str = Field(
        description="Guide for handling long-running operations"
    )
    custom_instructions: str = Field(
        default="", description="Additional custom usage instructions"
    )


# ============================================================================
# Database Table Models (SQLModel) - Used for SQLite storage
# ============================================================================


class SDKMethodTable(SQLModel, table=True):
    """SQLModel table for storing SDK method specifications."""

    __tablename__ = "sdk_methods"  # type: ignore[misc]  # pyright: ignore[reportAssignmentType]

    id: int | None = SQLField(default=None, primary_key=True)
    service_name: str = SQLField(index=True)
    class_name: str
    method_name: str
    full_name: str = SQLField(index=True)
    signature: str
    docstring: str | None = None
    rst_docs: str | None = None
    has_rst: bool = SQLField(default=False, index=True)
    parameters_json: str = SQLField(
        default="[]", description="JSON-serialized list of SDKParameterInfo"
    )

    def to_spec(self) -> SDKMethodSpec:
        """Convert database row to SDKMethodSpec response model."""
        import json

        params_data: list[dict[str, str | None]] = json.loads(self.parameters_json)
        parameters = [SDKParameterInfo.model_validate(p) for p in params_data]

        return SDKMethodSpec(
            service_name=self.service_name,
            class_name=self.class_name,
            method_name=self.method_name,
            full_name=self.full_name,
            signature=self.signature,
            docstring=self.docstring,
            rst_docs=self.rst_docs,
            has_rst=self.has_rst,
            parameters=parameters,
        )

    @classmethod
    def from_spec(
        cls, spec: SDKMethodSpec, row_id: int | None = None
    ) -> "SDKMethodTable":
        """Create database row from SDKMethodSpec."""
        import json

        return cls(
            id=row_id,
            service_name=spec.service_name,
            class_name=spec.class_name,
            method_name=spec.method_name,
            full_name=spec.full_name,
            signature=spec.signature,
            docstring=spec.docstring,
            rst_docs=spec.rst_docs,
            has_rst=spec.has_rst,
            parameters_json=json.dumps([p.model_dump() for p in spec.parameters]),
        )


class SDKModelTable(SQLModel, table=True):
    """SQLModel table for storing SDK model specifications."""

    __tablename__ = "sdk_models"  # type: ignore[misc]  # pyright: ignore[reportAssignmentType]

    id: int | None = SQLField(default=None, primary_key=True)
    module_name: str = SQLField(index=True)
    class_name: str
    full_name: str = SQLField(index=True)
    docstring: str | None = None
    fields_json: str = SQLField(
        default="[]", description="JSON-serialized list of SDKModelField"
    )

    def to_spec(self) -> SDKModelSpec:
        """Convert database row to SDKModelSpec response model."""
        import json

        fields_data: list[dict[str, str | None]] = json.loads(self.fields_json)
        model_fields = [SDKModelField.model_validate(f) for f in fields_data]

        return SDKModelSpec(
            module_name=self.module_name,
            class_name=self.class_name,
            full_name=self.full_name,
            docstring=self.docstring,
            fields=model_fields,
        )

    @classmethod
    def from_spec(
        cls, spec: SDKModelSpec, row_id: int | None = None
    ) -> "SDKModelTable":
        """Create database row from SDKModelSpec."""
        import json

        return cls(
            id=row_id,
            module_name=spec.module_name,
            class_name=spec.class_name,
            full_name=spec.full_name,
            docstring=spec.docstring,
            fields_json=json.dumps([f.model_dump() for f in spec.fields]),
        )

    @property
    def field_names(self) -> str:
        """Get space-separated field names for FTS5 indexing."""
        import json

        fields_data: list[dict[str, str | None]] = json.loads(self.fields_json)
        return " ".join(f.get("name") or "" for f in fields_data)
