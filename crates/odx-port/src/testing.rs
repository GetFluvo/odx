//! Test doubles shared by the unit tests.

use odx_core::system::OsRunner;
use odx_core::{Invocation, ProcessRunner};

/// Runs commands for real, except the `fail_at`-th, which fails to
/// start (`spawn`) or exits with 1.
#[derive(Debug)]
pub(crate) struct Faulty {
    fail_at: usize,
    spawn: bool,
    pub(crate) calls: std::sync::atomic::AtomicUsize,
}

impl Faulty {
    pub(crate) fn new(fail_at: usize, spawn: bool) -> Self {
        Self {
            fail_at,
            spawn,
            calls: std::sync::atomic::AtomicUsize::new(0),
        }
    }
}

impl ProcessRunner for Faulty {
    fn run(&self, invocation: &Invocation) -> std::io::Result<odx_core::Output> {
        let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if call != self.fail_at {
            return OsRunner.run(invocation);
        }
        if self.spawn {
            return Err(std::io::Error::other("injected"));
        }
        Ok(odx_core::Output {
            status: Some(1),
            ..odx_core::Output::default()
        })
    }
}
