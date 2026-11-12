//! Source picker provided by an external program.
//!
//! If `SHOJIWM_PORTAL_PICKER` is set and non-empty when the portal starts,
//! every pick runs that command through `sh -c` (so it may carry arguments)
//! instead of opening the built-in iced window. This lets a desktop built on
//! ShojiWM show a chooser that matches the rest of its UI.
//!
//! Protocol:
//! - The portal writes one JSON object to the child's stdin, then closes it:
//!
//!   ```json
//!   {
//!     "app_id": "<requesting app id, may be empty>",
//!     "prefer": "window" | "screen",
//!     "sources": [
//!       {"id": "DP-1", "kind": "output", "name": "DP-1", "description": "...",
//!        "x": 0, "y": 0, "width": 2560, "height": 1440, "refresh_mhz": 144000},
//!       {"id": "<toplevel identifier>", "kind": "window", "title": "...", "app_id": "..."}
//!     ]
//!   }
//!   ```
//!
//! - The child prints the id of the chosen source on the first line of
//!   stdout and exits with status 0.
//! - Any other outcome (non-zero exit, empty output, unknown id) cancels the
//!   request. If the session closes while the child runs, the child is killed.

use std::process::Stdio;

use serde_json::{Value, json};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::sync::watch;

use crate::picker::PickResult;
use crate::sources::{SourceInfo, SourceKind};

pub const ENV_VAR: &str = "SHOJIWM_PORTAL_PICKER";

/// Reads the command once at startup, so a picker cannot be swapped in later
/// through the environment of a running portal.
pub fn command_from_env() -> Option<String> {
    std::env::var(ENV_VAR)
        .ok()
        .filter(|cmd| !cmd.trim().is_empty())
}

pub async fn pick(
    command: &str,
    app_id: &str,
    sources: Vec<SourceInfo>,
    mut cancelled: watch::Receiver<bool>,
    prefer_windows: bool,
) -> PickResult {
    let request = request_json(app_id, &sources, prefer_windows);

    // kill_on_drop: SelectSources drops this future when the session closes,
    // and an orphaned chooser left on screen would answer nobody.
    let mut child = match Command::new("sh")
        .arg("-c")
        .arg(command)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
    {
        Ok(child) => child,
        Err(e) => {
            tracing::warn!(command, "external picker: spawn failed: {e}");
            return PickResult::Cancelled;
        }
    };

    let stdin = child.stdin.take();
    // Write concurrently with waiting: a request larger than the pipe buffer
    // would otherwise deadlock against a child that reads lazily.
    let write = async move {
        // Dropping stdin at the end closes it, so the child sees end of input.
        if let Some(mut stdin) = stdin
            && let Err(e) = stdin.write_all(&request).await
        {
            // The child may exit without reading its input; that is its
            // choice, the exit status still decides the result.
            tracing::debug!("external picker: writing request failed: {e}");
        }
    };
    let run = async {
        let ((), output) = tokio::join!(write, child.wait_with_output());
        output
    };

    let output = tokio::select! {
        biased;
        _ = cancelled.wait_for(|closed| *closed) => {
            // Leaving this arm drops `run` and with it the child: killed.
            tracing::info!("external picker: session cancelled; killing picker");
            return PickResult::Cancelled;
        }
        output = run => output,
    };

    let output = match output {
        Ok(output) => output,
        Err(e) => {
            tracing::warn!("external picker: waiting for the child failed: {e}");
            return PickResult::Cancelled;
        }
    };
    if !output.status.success() {
        tracing::info!(status = %output.status, "external picker: cancelled");
        return PickResult::Cancelled;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let chosen = stdout.lines().next().unwrap_or("").trim();
    if chosen.is_empty() {
        return PickResult::Cancelled;
    }
    match sources.into_iter().find(|source| source.id() == chosen) {
        Some(source) => PickResult::Source(source),
        None => {
            tracing::warn!(chosen, "external picker: returned an unknown source id");
            PickResult::Cancelled
        }
    }
}

fn request_json(app_id: &str, sources: &[SourceInfo], prefer_windows: bool) -> Vec<u8> {
    let sources: Vec<Value> = sources
        .iter()
        .map(|source| match &source.kind {
            SourceKind::Output(o) => json!({
                "id": source.id(),
                "kind": "output",
                "name": o.name,
                "description": o.description,
                "x": o.position.0,
                "y": o.position.1,
                "width": o.width,
                "height": o.height,
                "refresh_mhz": o.refresh_mhz,
            }),
            SourceKind::Toplevel(t) => json!({
                "id": source.id(),
                "kind": "window",
                "title": t.title,
                "app_id": t.app_id,
            }),
        })
        .collect();
    let request = json!({
        "app_id": app_id,
        "prefer": if prefer_windows { "window" } else { "screen" },
        "sources": sources,
    });
    let mut bytes = serde_json::to_vec(&request).expect("a JSON value always serializes");
    // A trailing newline lets line-based readers (shell `read`) take the request.
    bytes.push(b'\n');
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::{OutputInfo, ToplevelInfo};
    use std::time::Duration;
    use wayland_client::protocol::wl_output;

    fn sources() -> Vec<SourceInfo> {
        vec![
            SourceInfo {
                kind: SourceKind::Output(OutputInfo {
                    name: "DP-1".into(),
                    description: "Screen".into(),
                    position: (0, 0),
                    width: 1920,
                    height: 1080,
                    refresh_mhz: 60000,
                    transform: wl_output::Transform::Normal,
                }),
                thumbnail: None,
            },
            SourceInfo {
                kind: SourceKind::Toplevel(ToplevelInfo {
                    identifier: "win-1".into(),
                    title: "Notes".into(),
                    app_id: "foot".into(),
                }),
                thumbnail: None,
            },
        ]
    }

    async fn run(command: &str, cancel: bool) -> PickResult {
        let (tx, rx) = watch::channel(false);
        if cancel {
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(100)).await;
                let _ = tx.send(true);
            });
        } else {
            // Keep the sender alive: a dropped one is not a cancellation.
            std::mem::forget(tx);
        }
        // Bounded: a picker the portal fails to stop would hang the test.
        tokio::time::timeout(
            Duration::from_secs(10),
            pick(command, "org.example.App", sources(), rx, true),
        )
        .await
        .expect("the pick finishes")
    }

    #[tokio::test]
    async fn the_chosen_id_selects_its_source() {
        // The request reaches the child whole: it answers from what it read.
        let command = r#"read -r request; case "$request" in *'"prefer":"window"'*'"app_id":"foot"'*) echo win-1;; *) exit 3;; esac"#;
        match run(command, false).await {
            PickResult::Source(source) => assert_eq!(source.id(), "win-1"),
            PickResult::Cancelled => panic!("expected the window"),
        }
    }

    #[tokio::test]
    async fn a_failed_exit_cancels() {
        assert!(matches!(
            run("echo DP-1; exit 1", false).await,
            PickResult::Cancelled
        ));
    }

    #[tokio::test]
    async fn an_unknown_id_cancels() {
        assert!(matches!(
            run("echo nothing-like-it", false).await,
            PickResult::Cancelled
        ));
    }

    #[tokio::test]
    async fn a_cancelled_session_stops_the_picker() {
        assert!(matches!(
            run("exec sleep 30", true).await,
            PickResult::Cancelled
        ));
    }
}
