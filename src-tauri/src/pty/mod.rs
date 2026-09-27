pub mod manager;
pub mod registry;
pub mod session;
pub mod shell_integration;

use std::sync::{Mutex, MutexGuard};

/// Lock a mutex without panicking when it is poisoned.
///
/// None of this state has an invariant that a panic somewhere else could leave
/// half-updated in a way that matters, and a panic inside a PTY reader thread
/// would silently kill a terminal — so recover instead of propagating.
pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}
