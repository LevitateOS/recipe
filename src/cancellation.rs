//! Explicit CLI signal ownership; library callers keep their own handlers.
use anyhow::{Result, bail};
use std::sync::atomic::{AtomicI32, Ordering};
static SIGNAL: AtomicI32 = AtomicI32::new(0);
extern "C" fn interrupt(signal: i32) {
    SIGNAL.store(signal, Ordering::Relaxed);
}

/// Install coordinator interruption handling for the standalone CLI.
#[doc(hidden)]
pub fn install_cli_handlers() -> Result<()> {
    for signal in [libc::SIGINT, libc::SIGTERM] {
        let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
        action.sa_sigaction = interrupt as *const () as usize;
        unsafe {
            libc::sigemptyset(&mut action.sa_mask);
        }
        if unsafe { libc::sigaction(signal, &action, std::ptr::null_mut()) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    Ok(())
}
#[doc(hidden)]
pub fn signal() -> i32 {
    SIGNAL.load(Ordering::Relaxed)
}
pub(crate) fn check() -> Result<()> {
    if signal() != 0 {
        bail!(cancelled());
    }
    Ok(())
}

/// The error for an invocation interrupted by the latched signal.
pub(crate) fn cancelled() -> crate::failure::Failure {
    crate::failure::Category::Cancelled.error(format!("interrupted by signal {}", signal()))
}
pub(crate) fn check_io() -> std::io::Result<()> {
    check().map_err(std::io::Error::other)
}
