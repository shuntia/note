pub mod codec;
pub mod frame;
pub mod stream;

pub use frame::*;
pub use stream::{classify, Arrival, MemOutbox, Outbox};

/// A lock poisoned by a panic elsewhere still guards intact data.
pub(crate) fn lock<T>(m: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}
