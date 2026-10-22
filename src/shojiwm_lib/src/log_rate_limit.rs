//! Per-callsite rate limit for WARN and ERROR log lines.
//!
//! A warning that fires for every surface on every frame (smithay's import
//! errors when a client's buffer cannot be imported, for one) grew
//! `latest.log` past a gigabyte in a couple of minutes. This layer lets each
//! callsite log [`BURST`] lines per [`WINDOW`] and drops the rest, writing one
//! line when it starts dropping and one with the count when the window ends,
//! so the log still says what happened.
//!
//! INFO and below are never limited: those are the levels a `RUST_LOG` or
//! `SHOJI_*_DEBUG` setting turns on on purpose.
//!
//! `SHOJI_LOG_RATE_LIMIT=0` turns the limit off.

use std::{
    collections::HashMap,
    io::Write,
    sync::Mutex,
    time::{Duration, Instant},
};

use tracing::{Event, Level, Metadata, Subscriber, callsite::Identifier};
use tracing_subscriber::{
    fmt::{
        format::Writer,
        time::{FormatTime, SystemTime},
    },
    layer::{Context, Layer},
};

/// Lines one callsite may log per window.
pub const BURST: u32 = 100;
/// The window [`BURST`] counts over.
pub const WINDOW: Duration = Duration::from_secs(10);

struct CallsiteWindow {
    start: Instant,
    count: u32,
    dropped: u64,
}

pub struct LogRateLimitLayer<W> {
    /// Where the "dropping"/"dropped" notices go: the log file's writer.
    writer: W,
    burst: u32,
    window: Duration,
    callsites: Mutex<HashMap<Identifier, CallsiteWindow>>,
}

impl<W> LogRateLimitLayer<W>
where
    W: Write + Clone + 'static,
{
    pub fn new(writer: W) -> Self {
        Self::with_limits(writer, BURST, WINDOW)
    }

    fn with_limits(writer: W, burst: u32, window: Duration) -> Self {
        Self {
            writer,
            burst,
            window,
            callsites: Mutex::new(HashMap::new()),
        }
    }

    /// Count the event against its callsite. Returns whether it may be logged,
    /// and the notice to write first, if any.
    fn admit(&self, metadata: &'static Metadata<'static>, now: Instant) -> (bool, Option<String>) {
        let mut callsites = self
            .callsites
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let window = callsites
            .entry(metadata.callsite())
            .or_insert(CallsiteWindow {
                start: now,
                count: 0,
                dropped: 0,
            });
        let mut notice = None;
        if now.duration_since(window.start) >= self.window {
            if window.dropped > 0 {
                notice = Some(format!(
                    "log rate limit: dropped {} repeats of this line",
                    window.dropped
                ));
            }
            *window = CallsiteWindow {
                start: now,
                count: 0,
                dropped: 0,
            };
        }
        window.count += 1;
        if window.count <= self.burst {
            return (true, notice);
        }
        if window.dropped == 0 {
            notice = Some(format!(
                "log rate limit: more than {} lines in {}s, dropping repeats of this line until the window ends",
                self.burst,
                self.window.as_secs_f64()
            ));
        }
        window.dropped += 1;
        (false, notice)
    }

    fn write_notice(&self, metadata: &Metadata<'_>, notice: &str) {
        let mut line = String::new();
        let _ = SystemTime.format_time(&mut Writer::new(&mut line));
        line.push_str(&format!(
            "  WARN {}: {notice} target={} location={}:{}\n",
            module_path!(),
            metadata.target(),
            metadata.file().unwrap_or("?"),
            metadata.line().unwrap_or(0),
        ));
        let _ = self.writer.clone().write_all(line.as_bytes());
    }
}

impl<S, W> Layer<S> for LogRateLimitLayer<W>
where
    S: Subscriber,
    W: Write + Clone + 'static,
{
    fn event_enabled(&self, event: &Event<'_>, _ctx: Context<'_, S>) -> bool {
        let metadata = event.metadata();
        // More verbose levels compare greater: this lets INFO, DEBUG and TRACE through.
        if *metadata.level() > Level::WARN {
            return true;
        }
        let (allowed, notice) = self.admit(metadata, Instant::now());
        if let Some(notice) = notice {
            self.write_notice(metadata, &notice);
        }
        allowed
    }
}

/// False when `SHOJI_LOG_RATE_LIMIT=0` asks for every line.
pub fn enabled_from_env() -> bool {
    !matches!(
        std::env::var("SHOJI_LOG_RATE_LIMIT").as_deref(),
        Ok("0") | Ok("false") | Ok("off")
    )
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use tracing_subscriber::layer::SubscriberExt;

    use super::*;

    #[derive(Clone, Default)]
    struct Buffer(Arc<Mutex<Vec<u8>>>);

    impl Write for Buffer {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl Buffer {
        fn lines(&self) -> Vec<String> {
            String::from_utf8(self.0.lock().unwrap().clone())
                .unwrap()
                .lines()
                .map(str::to_owned)
                .collect()
        }
    }

    fn with_limited_log(burst: u32, window: Duration, body: impl FnOnce()) -> Vec<String> {
        let buffer = Buffer::default();
        let make_writer = {
            let buffer = buffer.clone();
            move || buffer.clone()
        };
        let subscriber = tracing_subscriber::registry()
            .with(LogRateLimitLayer::with_limits(
                buffer.clone(),
                burst,
                window,
            ))
            .with(
                tracing_subscriber::fmt::layer()
                    .compact()
                    .with_ansi(false)
                    .with_writer(make_writer),
            );
        tracing::subscriber::with_default(subscriber, body);
        buffer.lines()
    }

    #[test]
    fn repeats_past_the_burst_are_dropped_with_a_notice() {
        let lines = with_limited_log(3, Duration::from_secs(60), || {
            for index in 0..10 {
                tracing::warn!(index, "flood");
            }
        });
        let floods = lines.iter().filter(|line| line.contains("flood")).count();
        assert_eq!(floods, 3, "{lines:#?}");
        let notices = lines
            .iter()
            .filter(|line| line.contains("dropping repeats"))
            .count();
        assert_eq!(notices, 1, "{lines:#?}");
    }

    #[test]
    fn the_next_window_reports_the_dropped_count() {
        let lines = with_limited_log(2, Duration::from_millis(50), || {
            let flood = || {
                for _ in 0..5 {
                    tracing::error!("flood");
                }
            };
            flood();
            std::thread::sleep(Duration::from_millis(80));
            flood();
        });
        assert!(
            lines.iter().any(|line| line.contains("dropped 3 repeats")),
            "{lines:#?}"
        );
        let floods = lines.iter().filter(|line| line.ends_with("flood")).count();
        assert_eq!(floods, 4, "{lines:#?}");
    }

    #[test]
    fn info_and_other_callsites_are_not_limited() {
        let lines = with_limited_log(2, Duration::from_secs(60), || {
            for _ in 0..5 {
                tracing::info!("chatty info");
                tracing::warn!("warn a");
            }
            tracing::warn!("warn b");
        });
        assert_eq!(
            lines
                .iter()
                .filter(|line| line.contains("chatty info"))
                .count(),
            5
        );
        assert_eq!(
            lines.iter().filter(|line| line.contains("warn a")).count(),
            2
        );
        assert_eq!(
            lines.iter().filter(|line| line.contains("warn b")).count(),
            1
        );
    }
}
