//! Python-visible types exported via PyO3.
//!
//! ASGI primitives are registered into the `apx._core` extension module.
//! Users raise `fastapi.HTTPException` directly for HTTP error responses.

use pyo3::prelude::*;
use pyo3::types::PyModule;

/// Register framework types into the `apx._core` extension module.
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<crate::bridge::asgi::AsgiReceive>()?;
    m.add_class::<crate::bridge::asgi::AsgiSend>()?;

    // Scheduler core
    m.add_class::<crate::scheduler::adapters::anyio_backend::ApxSchedulerCore>()?;
    m.add_class::<crate::scheduler::adapters::cancel_scope::CancelScopeState>()?;
    m.add_class::<crate::scheduler::adapters::task_group::TaskGroupCore>()?;

    // Primitives
    m.add_class::<crate::scheduler::primitives::Event>()?;
    m.add_class::<crate::scheduler::primitives::EventWaiter>()?;
    m.add_class::<crate::scheduler::primitives::Lock>()?;
    m.add_class::<crate::scheduler::primitives::LockGuardFuture>()?;
    m.add_class::<crate::scheduler::primitives::LockGuard>()?;
    m.add_class::<crate::scheduler::primitives::Semaphore>()?;
    m.add_class::<crate::scheduler::primitives::SemaphoreAcquire>()?;
    m.add_class::<crate::scheduler::primitives::SemaphorePermit>()?;
    m.add_class::<crate::scheduler::primitives::Future>()?;
    m.add_class::<crate::scheduler::primitives::Timer>()?;
    m.add_class::<crate::scheduler::primitives::BlockingTask>()?;
    m.add_class::<crate::scheduler::primitives::CancelToken>()?;

    Ok(())
}
