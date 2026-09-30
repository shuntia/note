pub mod codec;
pub mod frame;
pub mod journal;
pub mod peer;
pub mod stream;
pub mod testkit;

pub use frame::*;
pub use journal::{AppliedFile, FileOutbox};
pub use peer::{dial_forever, listen_forever, BoxFuture, Disconnect, Handler, Peer, PeerConfig};
pub use stream::{classify, Arrival, MemOutbox, Outbox};

/// A lock poisoned by a panic elsewhere still guards intact data.
pub(crate) fn lock<T>(m: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}
