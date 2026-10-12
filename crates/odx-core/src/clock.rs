use std::time::SystemTime;

/// Source of the current time.
///
/// Used for cache staleness, verdict expiry and lockfile timestamps, so
/// tests can control time.
pub trait Clock: Send + Sync {
    /// The current wall-clock time.
    fn now(&self) -> SystemTime;
}
