"""Agent protocol addon — re-exports from apx_agent package.

Define agent tools as plain typed functions and register them with Agent():

    from .core import Dependencies
    from .core.agent import Agent

    def query_genie(question: str, space_id: str, ws: Dependencies.UserClient) -> str:
        \"\"\"Answer a natural language question using a Genie Space.\"\"\"
        return ws.genie.ask(space_id=space_id, question=question).answer or ""

    agent = Agent(tools=[query_genie])
"""

# Core runtime (from apx-agent package)
from apx_agent import (  # noqa: F401
    Agent,
    AgentCard,
    AgentConfig,
    AgentContext,
    AgentTool,
    AfterToolHook,
    BaseAgent,
    BeforeToolHook,
    HandoffAgent,
    InputGuardrailFn,
    InvocationRequest,
    InvocationResponse,
    LlmAgent,
    LoopAgent,
    Message,
    OutputGuardrailFn,
    ParallelAgent,
    RouterAgent,
    SequentialAgent,
    app_predict_fn,
    create_app,
    set_custom_output,
    setup_agent,
)
from apx_agent._models import (  # noqa: F401
    A2ACapabilities,
    A2AAuthScheme,
    A2AProvider,
    A2ASkill,
    OutputItem,
    OutputTextContent,
    _ToolFn,
    _get_agent_instance,
    _set_agent_instance,
)

# Dependency (FastAPI lifespan bridge)
from ._agent._dependency import _AgentDependency  # noqa: F401
AgentDependency = _AgentDependency
