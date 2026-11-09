//! Locking shared by the server's queues and caches.

use std::sync::{Mutex, MutexGuard, PoisonError};

/// Locks `mutex`, taking the data of a poisoned lock as it is.
///
/// A panic on one worker poisons every mutex it held. The guarded data are
/// maps, queues and counters that every critical section leaves consistent,
/// so recovering them keeps the server answering instead of failing each
/// later request with the same panic.
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
