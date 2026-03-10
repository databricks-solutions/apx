//! [`IoHandle`] — stub for future I/O integration.

use pyo3::prelude::*;

/// Placeholder for future I/O integration.
///
/// Will wrap `AsyncFd` or `TcpStream` in a future phase. Currently a
/// forward-compatibility stub.
#[derive(Debug)]
#[pyclass(module = "apx._core")]
pub struct IoHandle {
    _private: (),
}

#[pymethods]
impl IoHandle {
    #[new]
    fn new() -> Self {
        Self { _private: () }
    }

    #[allow(
        clippy::unused_self,
        reason = "__repr__ requires &self per Python protocol"
    )]
    fn __repr__(&self) -> &'static str {
        "IoHandle(stub)"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn io_handle_repr() {
        let handle = IoHandle::new();
        assert_eq!(handle.__repr__(), "IoHandle(stub)");
    }
}
