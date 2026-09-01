//! Panic capture — issue #88 wants a panic signature + minimized reproducer
//! path per crash, not a killed process. The default panic hook only prints
//! to stderr; we install a hook that additionally stashes the message +
//! location in a process-wide slot, then wrap every pipeline stage in
//! `catch_unwind` so one malformed document never aborts the batch.
//!
//! `AssertUnwindSafe` is used at every `catch_unwind` call site: the engine's
//! `im::Vector`-backed `DocumentTree` and friends aren't `UnwindSafe` by the
//! compiler's conservative default (interior mutability nowhere in this
//! read-only walk actually panics mid-mutation), and we never touch a
//! caught-panicking value again after unwinding — a fresh call is issued per
//! document, per stage.

use std::panic::{self, AssertUnwindSafe};
use std::sync::{Mutex, OnceLock};

/// One captured panic: message + `file:line:col`.
#[derive(Debug, Clone)]
pub struct CaughtPanic {
    pub message: String,
    pub location: String,
}

static LAST_PANIC: OnceLock<Mutex<Option<CaughtPanic>>> = OnceLock::new();

/// Install the process-wide panic hook. Call once at startup, before any
/// `catch_unwind`. Suppresses the default stderr dump (`RUST_BACKTRACE`
/// still works if a caller wants it) since a corpus run expects thousands of
/// caught panics — printing every one would drown the progress log.
pub fn install() {
    let slot = LAST_PANIC.get_or_init(|| Mutex::new(None));
    panic::set_hook(Box::new(move |info| {
        let message = match info.payload().downcast_ref::<&str>() {
            Some(s) => (*s).to_string(),
            None => match info.payload().downcast_ref::<String>() {
                Some(s) => s.clone(),
                None => "<non-string panic payload>".to_string(),
            },
        };
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "<unknown location>".to_string());
        if let Ok(mut guard) = slot.lock() {
            *guard = Some(CaughtPanic { message, location });
        }
    }));
}

/// Run `f`, catching a panic. `Ok` on normal return; `Err(CaughtPanic)` with
/// the message + location the hook captured (falls back to a generic
/// message if the hook slot is somehow empty — e.g. a panic-in-a-panic).
pub fn catch<T>(f: impl FnOnce() -> T) -> Result<T, CaughtPanic> {
    match panic::catch_unwind(AssertUnwindSafe(f)) {
        Ok(v) => Ok(v),
        Err(_payload) => {
            let captured = LAST_PANIC
                .get()
                .and_then(|m| m.lock().ok())
                .and_then(|mut g| g.take());
            Err(captured.unwrap_or_else(|| CaughtPanic {
                message: "<panic captured with no hook data>".to_string(),
                location: "<unknown location>".to_string(),
            }))
        }
    }
}

/// Normalize a panic (or error) message into a stable *signature* for
/// bucketing: digit runs collapse to `N` so "index out of bounds: the len is
/// 5 but the index is 42" and "...len is 512 but the index is 9001" land in
/// the same bucket, and the message is capped so pathological giant panic
/// strings don't blow up the JSONL.
pub fn normalize_signature(stage: &str, location: &str, message: &str) -> String {
    let mut normalized = String::with_capacity(message.len());
    let mut chars = message.chars().peekable();
    while let Some(c) = chars.next() {
        if c.is_ascii_digit() {
            normalized.push('N');
            while chars.peek().is_some_and(char::is_ascii_digit) {
                chars.next();
            }
        } else {
            normalized.push(c);
        }
    }
    if normalized.len() > 200 {
        normalized.truncate(200);
        normalized.push_str("...");
    }
    format!("{stage}@{location}: {normalized}")
}
