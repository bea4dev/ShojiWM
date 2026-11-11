//! `SHOJI_BACKTRACE_SIGNAL=1`: `kill -USR2 <pid>` prints the backtrace of the
//! thread that takes the signal (the main thread, which runs the event loop,
//! unless it blocks SIGUSR2) to stderr. For a compositor that has stopped
//! answering where ptrace is not allowed.
//!
//! Capturing a backtrace allocates and takes locks, which a signal handler
//! must not do; it is acceptable here because the process is already stuck
//! and the flag is off by default.

use std::backtrace::Backtrace;
use std::io::Write;

pub fn install_if_enabled() {
    if !std::env::var_os("SHOJI_BACKTRACE_SIGNAL").is_some_and(|value| value == "1") {
        return;
    }
    extern "C" fn dump(_: libc::c_int) {
        let backtrace = Backtrace::force_capture();
        let thread = std::thread::current();
        let _ = writeln!(
            std::io::stderr(),
            "SIGUSR2 backtrace of thread {:?} ({}):\n{backtrace}",
            thread.name().unwrap_or("unnamed"),
            unsafe { libc::gettid() },
        );
    }
    // Safety: `dump` has the signature sigaction expects, and SA_RESTART
    // lets the interrupted syscall carry on afterwards.
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = dump as extern "C" fn(libc::c_int) as libc::sighandler_t;
        action.sa_flags = libc::SA_RESTART;
        libc::sigemptyset(&mut action.sa_mask);
        libc::sigaction(libc::SIGUSR2, &action, std::ptr::null_mut());
    }
    tracing::info!(pid = std::process::id(), "SIGUSR2 prints a backtrace");
}
