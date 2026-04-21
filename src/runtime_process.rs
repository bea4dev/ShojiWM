use std::{
    collections::BTreeMap,
    io,
    process::{Child, Command, ExitStatus, Stdio},
};

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

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(untagged)]
pub enum RuntimeProcessLaunch {
    Command { command: Vec<String> },
    Shell { shell: String },
}

impl RuntimeProcessLaunch {
    pub fn is_valid(&self) -> bool {
        match self {
            Self::Command { command } => !command.is_empty(),
            Self::Shell { shell } => !shell.trim().is_empty(),
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

    // Child processes launched by ShojiWM should identify this compositor session,
    // not whichever compositor environment may have existed before ShojiWM started.
    command.env("XDG_CURRENT_DESKTOP", "ShojiWM");
    command.env("XDG_SESSION_DESKTOP", "ShojiWM");
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
        RuntimeProcessLaunch::Shell { shell } => {
            if shell.trim().is_empty() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "process shell command must not be empty",
                ));
            }
            let mut cmd = Command::new("/bin/sh");
            cmd.arg("-lc").arg(shell);
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
