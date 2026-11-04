use std::{
    collections::BTreeMap,
    io,
    process::{Child, Command, ExitStatus, Stdio},
    time::{Duration, Instant},
};

const SERVICE_SPAWN_RETRY_INITIAL: Duration = Duration::from_secs(1);
const SERVICE_SPAWN_RETRY_MAX: Duration = Duration::from_secs(30);

/// When to try again to start a service whose executable could not be started.
///
/// Spawning happens on the main thread and costs a fraction of a millisecond even when it
/// fails. A command that is not installed fails the same way every time, so retrying it
/// without pause costs frames for as long as the session lasts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServiceSpawnBackoff {
    next_attempt_at: Instant,
    delay: Duration,
}

impl ServiceSpawnBackoff {
    pub fn first_failure(now: Instant) -> Self {
        Self {
            next_attempt_at: now + SERVICE_SPAWN_RETRY_INITIAL,
            delay: SERVICE_SPAWN_RETRY_INITIAL,
        }
    }

    pub fn next_failure(self, now: Instant) -> Self {
        let delay = (self.delay * 2).min(SERVICE_SPAWN_RETRY_MAX);
        Self {
            next_attempt_at: now + delay,
            delay,
        }
    }

    pub fn is_due(&self, now: Instant) -> bool {
        now >= self.next_attempt_at
    }

    pub fn delay(&self) -> Duration {
        self.delay
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct RuntimeProcessConfigUpdate {
    pub entries: Vec<RuntimeProcessEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum RuntimeProcessEntry {
    Once {
        id: String,
        #[serde(flatten)]
        launch: RuntimeProcessLaunch,
        cwd: Option<String>,
        #[serde(default)]
        env: BTreeMap<String, String>,
        #[serde(rename = "runPolicy", default)]
        run_policy: RuntimeProcessRunPolicy,
    },
    Service {
        id: String,
        #[serde(flatten)]
        launch: RuntimeProcessLaunch,
        cwd: Option<String>,
        #[serde(default)]
        env: BTreeMap<String, String>,
        #[serde(default)]
        restart: RuntimeProcessRestartPolicy,
        #[serde(default)]
        reload: RuntimeProcessReloadPolicy,
    },
}

impl RuntimeProcessEntry {
    pub fn id(&self) -> &str {
        match self {
            Self::Once { id, .. } | Self::Service { id, .. } => id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct RuntimeProcessAction {
    #[serde(flatten)]
    pub launch: RuntimeProcessLaunch,
    pub cwd: Option<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

/// How a runtime process should be launched.
///
/// Distinguished purely by the *type* of the `command` JSON field:
/// - a string is interpreted as a shell command line (`/bin/sh -lc <command>`),
///   which is what you want when the command needs pipes, redirection, or
///   environment expansion.
/// - an array of strings is exec'd directly with no shell involvement.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(untagged)]
pub enum RuntimeProcessLaunch {
    Shell { command: String },
    Command { command: Vec<String> },
}

impl RuntimeProcessLaunch {
    pub fn is_valid(&self) -> bool {
        match self {
            Self::Command { command } => !command.is_empty(),
            Self::Shell { command } => !command.trim().is_empty(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum RuntimeProcessRunPolicy {
    #[default]
    OncePerSession,
    OncePerConfigVersion,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum RuntimeProcessRestartPolicy {
    Never,
    OnFailure,
    #[default]
    OnExit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum RuntimeProcessReloadPolicy {
    #[default]
    KeepIfUnchanged,
    AlwaysRestart,
}

#[derive(Debug)]
pub struct ManagedRuntimeService {
    pub spec: RuntimeProcessEntry,
    pub child: Child,
    pub last_started_generation: u64,
}

fn prepare_runtime_process_environment(command: &mut Command) {
    for key in [
        "NIRI_SOCKET",
        "HYPRLAND_INSTANCE_SIGNATURE",
        "SWAYSOCK",
        "I3SOCK",
        "LABWC_PID",
    ] {
        command.env(key, "");
    }

    command.env("XDG_CURRENT_DESKTOP", "ShojiWM");
    command.env("XDG_SESSION_DESKTOP", "ShojiWM");
    command.env("XDG_SESSION_TYPE", "wayland");
    command.env("DESKTOP_SESSION", "ShojiWM");
}

pub fn spawn_runtime_process(
    launch: &RuntimeProcessLaunch,
    cwd: Option<&str>,
    env: &BTreeMap<String, String>,
) -> io::Result<Child> {
    let mut command = match launch {
        RuntimeProcessLaunch::Command { command } => {
            if command.is_empty() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "process command must not be empty",
                ));
            }
            let mut cmd = Command::new(&command[0]);
            if command.len() > 1 {
                cmd.args(&command[1..]);
            }
            cmd
        }
        RuntimeProcessLaunch::Shell { command } => {
            if command.trim().is_empty() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "process shell command must not be empty",
                ));
            }
            let mut cmd = Command::new("/bin/sh");
            cmd.arg("-lc").arg(command);
            cmd
        }
    };

    prepare_runtime_process_environment(&mut command);

    if let Some(cwd) = cwd.filter(|cwd| !cwd.is_empty()) {
        command.current_dir(cwd);
    }
    if !env.is_empty() {
        command.envs(env);
    }

    command.stdin(Stdio::null());
    command.stdout(Stdio::null());
    command.stderr(Stdio::null());
    command.spawn()
}

pub fn kill_runtime_service(service: &mut ManagedRuntimeService) -> io::Result<()> {
    match service.child.try_wait()? {
        Some(_) => Ok(()),
        None => {
            service.child.kill()?;
            let _ = service.child.wait();
            Ok(())
        }
    }
}

pub fn should_restart_service(policy: RuntimeProcessRestartPolicy, status: ExitStatus) -> bool {
    match policy {
        RuntimeProcessRestartPolicy::Never => false,
        RuntimeProcessRestartPolicy::OnExit => true,
        RuntimeProcessRestartPolicy::OnFailure => !status.success(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_spawn_waits_before_the_next_attempt() {
        let now = Instant::now();
        let backoff = ServiceSpawnBackoff::first_failure(now);
        assert!(!backoff.is_due(now));
        assert!(!backoff.is_due(now + Duration::from_millis(999)));
        assert!(backoff.is_due(now + SERVICE_SPAWN_RETRY_INITIAL));
    }

    #[test]
    fn repeated_failures_back_off_up_to_the_limit() {
        let now = Instant::now();
        let mut backoff = ServiceSpawnBackoff::first_failure(now);
        let mut delays = vec![backoff.delay().as_secs()];
        for _ in 0..7 {
            backoff = backoff.next_failure(now);
            delays.push(backoff.delay().as_secs());
        }
        assert_eq!(delays, [1, 2, 4, 8, 16, 30, 30, 30]);
        assert!(!backoff.is_due(now + Duration::from_secs(29)));
        assert!(backoff.is_due(now + SERVICE_SPAWN_RETRY_MAX));
    }
}
