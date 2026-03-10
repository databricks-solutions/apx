from __future__ import annotations

from apx._backend._backend import ApxBackend
from apx._backend._cancel_scope import ApxCancelScope, _task_states
from apx._backend._memory_stream import create_memory_object_stream_pair
from apx._backend._task_group import ApxTaskGroup
from apx._core import ApxSchedulerCore


def create_backend(core: ApxSchedulerCore) -> ApxBackend:
    return ApxBackend(
        core=core,
        cancel_scope_cls=ApxCancelScope,
        task_group_cls=ApxTaskGroup,
        create_stream_pair=create_memory_object_stream_pair,
        task_states=_task_states,
    )
