//! System power through systemd-logind, for configs that decide what the lid
//! does (`COMPOSITOR.power`).
//!
//! logind reads the lid switch itself and suspends on its own, in no defined
//! order with the compositor, which hears of the lid through libinput. A config
//! that wants the lid's last word takes it from logind: ShojiWM then holds a
//! `handle-lid-switch` inhibitor, logind leaves the lid alone, and the config
//! asks for a suspend (or another action) when it wants one. The inhibitor is a
//! file descriptor, so logind gets the lid back by itself if ShojiWM goes away.
//!
//! Also here: whether the machine runs on external power (what logind's
//! `HandleLidSwitchExternalPower` is about), read from `power_supply` devices
//! and watched through udev, and the end of a suspend (`PrepareForSleep`).
//! Only the TTY backend talks to logind; a nested session's lid and power
//! belong to the compositor it runs in.

use std::path::Path;

use smithay::reexports::calloop::{
    Interest, LoopHandle, Mode, PostAction, channel::Event as ChannelEvent, generic::Generic,
};
use tracing::{info, warn};

use crate::state::ShojiWM;

const POWER_SUPPLY_CLASS: &str = "/sys/class/power_supply";

/// What logind does about the lid, by situation: its `HandleLidSwitch`,
/// `HandleLidSwitchExternalPower` and `HandleLidSwitchDocked` settings, as
/// action names (`suspend`, `ignore`, ...).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogindLidActions {
    pub lid: String,
    pub external_power: String,
    pub docked: String,
}

/// The power state the config sees (`COMPOSITOR.power.state`).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PowerStateSnapshot {
    /// Whether the machine runs on external power; `None` until read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub external_power: Option<bool>,
    /// `None` until logind answered, and in a nested session.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logind_lid_actions: Option<LogindLidActions>,
}

/// The machine came back from a suspend or hibernation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResumeSnapshot {
    /// What the lid switch reported last, which a broken switch may still
    /// report as closed; `None` without a lid switch.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lid_closed: Option<bool>,
}

/// A logind action, named as in `logind.conf`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerAction {
    Suspend,
    Hibernate,
    HybridSleep,
    SuspendThenHibernate,
    /// Whichever sleep logind is configured for (systemd 256 and later; a
    /// suspend before that).
    Sleep,
    /// Ask every session's screen locker to lock (`loginctl lock-sessions`).
    Lock,
    PowerOff,
    Reboot,
    Halt,
}

impl PowerAction {
    /// The action named `name`, or `None` for `ignore` and for actions the
    /// compositor does not run on the config's behalf (`kexec`,
    /// `factory-reset`, `secure-attention-key`).
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "suspend" => Some(Self::Suspend),
            "hibernate" => Some(Self::Hibernate),
            "hybrid-sleep" => Some(Self::HybridSleep),
            "suspend-then-hibernate" => Some(Self::SuspendThenHibernate),
            "sleep" => Some(Self::Sleep),
            "lock" => Some(Self::Lock),
            "poweroff" => Some(Self::PowerOff),
            "reboot" => Some(Self::Reboot),
            "halt" => Some(Self::Halt),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Suspend => "suspend",
            Self::Hibernate => "hibernate",
            Self::HybridSleep => "hybrid-sleep",
            Self::SuspendThenHibernate => "suspend-then-hibernate",
            Self::Sleep => "sleep",
            Self::Lock => "lock",
            Self::PowerOff => "poweroff",
            Self::Reboot => "reboot",
            Self::Halt => "halt",
        }
    }
}

/// A power request from the config runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimePowerRequest {
    /// What the config takes from logind. Every config that wants the lid says
    /// so again after a reload; one that does not gives it back.
    TakeOver {
        lid: bool,
    },
    Perform(PowerAction),
}

/// The power state the compositor keeps.
#[derive(Debug, Default)]
pub struct PowerState {
    logind: Option<logind::Client>,
    /// Whether the compositor holds the lid (asked for by the config).
    lid_taken: bool,
    /// Whether the config running now asked for the lid, or for nothing. Unset
    /// while a reloaded config has not said yet.
    declared: bool,
    external_power: Option<bool>,
    logind_lid_actions: Option<LogindLidActions>,
}

impl PowerState {
    fn snapshot(&self) -> PowerStateSnapshot {
        PowerStateSnapshot {
            external_power: self.external_power,
            logind_lid_actions: self.logind_lid_actions.clone(),
        }
    }
}

/// Talk to logind and watch the power supplies. For the TTY backend.
pub fn start(state: &mut ShojiWM, loop_handle: &LoopHandle<'static, ShojiWM>) {
    state.power.external_power = Some(read_external_power(Path::new(POWER_SUPPLY_CLASS)));
    watch_power_supplies(loop_handle);
    match logind::Client::start(loop_handle) {
        Ok(client) => state.power.logind = Some(client),
        Err(error) => warn!(%error, "failed to start the logind client"),
    }
    state.sync_power_state();
}

fn watch_power_supplies(loop_handle: &LoopHandle<'static, ShojiWM>) {
    let socket = match smithay::reexports::udev::MonitorBuilder::new()
        .and_then(|builder| builder.match_subsystem("power_supply"))
        .and_then(|builder| builder.listen())
    {
        Ok(socket) => socket,
        Err(error) => {
            warn!(%error, "failed to watch power supplies; external power will not be followed");
            return;
        }
    };
    let source = Generic::new(socket, Interest::READ, Mode::Level);
    let inserted = loop_handle.insert_source(source, |_, socket, state| {
        // Batteries report a change every few percent; reading the supplies is
        // cheap, and the snapshot only goes out when the answer changes.
        if socket.iter().count() > 0 {
            state.power.external_power = Some(read_external_power(Path::new(POWER_SUPPLY_CLASS)));
            state.sync_power_state();
        }
        Ok(PostAction::Continue)
    });
    if let Err(error) = inserted {
        warn!(%error, "failed to register the power supply monitor");
    }
}

/// One `power_supply` device, as far as external power is concerned.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PowerSupply {
    kind: String,
    online: Option<bool>,
    /// `Device` for the battery of a peripheral (a mouse, a headset).
    scope: Option<String>,
}

/// Whether the machine runs on external power, as systemd decides it
/// (`on_ac_power`): an adapter that is online says yes; adapters that are all
/// offline say no; no adapter at all (a desktop) says yes.
fn on_external_power(supplies: &[PowerSupply]) -> bool {
    let mut offline = false;
    for supply in supplies {
        if supply.scope.as_deref() == Some("Device") || supply.kind == "Battery" {
            continue;
        }
        match supply.online {
            Some(true) => return true,
            Some(false) => offline = true,
            None => {}
        }
    }
    !offline
}

fn read_external_power(class: &Path) -> bool {
    let read = |path: &Path, name: &str| {
        std::fs::read_to_string(path.join(name))
            .ok()
            .map(|value| value.trim().to_owned())
    };
    let supplies = std::fs::read_dir(class)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            Some(PowerSupply {
                kind: read(&path, "type")?,
                online: read(&path, "online").map(|online| online == "1"),
                scope: read(&path, "scope"),
            })
        })
        .collect::<Vec<_>>();
    on_external_power(&supplies)
}

impl ShojiWM {
    pub fn apply_runtime_power_request(&mut self, request: RuntimePowerRequest) {
        match request {
            RuntimePowerRequest::TakeOver { lid } => {
                self.power.declared = true;
                self.set_lid_taken(lid);
            }
            RuntimePowerRequest::Perform(action) => match &self.power.logind {
                Some(logind) => {
                    info!(action = action.name(), "asking logind for a power action");
                    logind.send(logind::Command::Perform(action));
                }
                None => warn!(
                    action = action.name(),
                    "no logind connection (nested session?); ignoring the power action"
                ),
            },
        }
    }

    fn set_lid_taken(&mut self, lid: bool) {
        if self.power.lid_taken == lid {
            return;
        }
        self.power.lid_taken = lid;
        match &self.power.logind {
            Some(logind) => {
                info!(lid, "lid switch handling taken from logind");
                logind.send(logind::Command::HoldLid(lid));
            }
            None if lid => {
                info!("no logind connection (nested session?); the lid stays with logind")
            }
            None => {}
        }
    }

    /// A reloaded config is about to set itself up. Until it asks for the lid
    /// again, the old config's hold stays, so logind never sees a gap.
    pub(crate) fn power_reload_starting(&mut self) {
        self.power.declared = false;
    }

    /// The reloaded config is set up (or failed to). Whatever it did not ask
    /// for goes back to logind; a config that failed to load handles nothing.
    pub(crate) fn power_reload_finished(&mut self, loaded: bool) {
        if !loaded || !self.power.declared {
            self.set_lid_taken(false);
        }
        self.power.declared = true;
    }

    /// Send the power state to the config when it changed.
    pub fn sync_power_state(&mut self) {
        if self.config_runtime.sync_power_state(self.power.snapshot()) {
            self.runtime_scheduler_enabled = true;
        }
    }

    fn handle_logind_event(&mut self, event: logind::Event) {
        match event {
            logind::Event::LidActions(actions) => {
                info!(?actions, "logind lid settings");
                self.power.logind_lid_actions = Some(actions);
                self.sync_power_state();
            }
            logind::Event::PrepareForSleep(true) => info!("the system is going to sleep"),
            logind::Event::PrepareForSleep(false) => self.system_resumed(),
        }
    }

    /// The system is back from a suspend or hibernation: tell the config.
    pub fn system_resumed(&mut self) {
        info!(lid_closed = ?self.lid_closed, "the system woke up");
        self.config_runtime.post_resume(ResumeSnapshot {
            lid_closed: self.lid_closed,
        });
        self.runtime_scheduler_enabled = true;
    }

    /// Whether the config took the lid switch from logind (held for real only
    /// with a logind connection, in the TTY backend).
    pub fn lid_taken_from_logind(&self) -> bool {
        self.power.lid_taken
    }
}

mod logind {
    //! The logind client, on a thread of its own: zbus needs an async runtime,
    //! and a call must never hold up the compositor.

    use std::{future::poll_fn, os::fd::OwnedFd, pin::Pin};

    use smithay::reexports::calloop::{LoopHandle, channel};
    use tokio::sync::mpsc;
    use tracing::{debug, info, warn};
    use zbus::export::futures_core::Stream;

    use super::{ChannelEvent, LogindLidActions, PowerAction};
    use crate::state::ShojiWM;

    #[zbus::proxy(
        interface = "org.freedesktop.login1.Manager",
        default_service = "org.freedesktop.login1",
        default_path = "/org/freedesktop/login1"
    )]
    trait Manager {
        fn inhibit(
            &self,
            what: &str,
            who: &str,
            why: &str,
            mode: &str,
        ) -> zbus::Result<zbus::zvariant::OwnedFd>;
        fn suspend(&self, interactive: bool) -> zbus::Result<()>;
        fn hibernate(&self, interactive: bool) -> zbus::Result<()>;
        fn hybrid_sleep(&self, interactive: bool) -> zbus::Result<()>;
        fn suspend_then_hibernate(&self, interactive: bool) -> zbus::Result<()>;
        fn sleep(&self, flags: u64) -> zbus::Result<()>;
        fn lock_sessions(&self) -> zbus::Result<()>;
        fn power_off(&self, interactive: bool) -> zbus::Result<()>;
        fn reboot(&self, interactive: bool) -> zbus::Result<()>;
        fn halt(&self, interactive: bool) -> zbus::Result<()>;
        #[zbus(signal)]
        fn prepare_for_sleep(&self, start: bool) -> zbus::Result<()>;
        #[zbus(property)]
        fn handle_lid_switch(&self) -> zbus::Result<String>;
        #[zbus(property)]
        fn handle_lid_switch_external_power(&self) -> zbus::Result<String>;
        #[zbus(property)]
        fn handle_lid_switch_docked(&self) -> zbus::Result<String>;
    }

    #[derive(Debug)]
    pub(super) enum Command {
        /// Hold (or let go of) the `handle-lid-switch` inhibitor.
        HoldLid(bool),
        Perform(PowerAction),
    }

    #[derive(Debug)]
    pub(super) enum Event {
        LidActions(LogindLidActions),
        /// `true` before a suspend, `false` once the system is back.
        PrepareForSleep(bool),
    }

    #[derive(Debug)]
    pub(super) struct Client {
        commands: mpsc::UnboundedSender<Command>,
    }

    impl Client {
        pub(super) fn start(
            loop_handle: &LoopHandle<'static, ShojiWM>,
        ) -> Result<Self, Box<dyn std::error::Error>> {
            let (events, event_receiver) = channel::channel::<Event>();
            loop_handle
                .insert_source(event_receiver, |event, _, state| {
                    if let ChannelEvent::Msg(event) = event {
                        state.handle_logind_event(event);
                    }
                })
                .map_err(|error| error.error)?;
            let (commands, command_receiver) = mpsc::unbounded_channel();
            std::thread::Builder::new()
                .name("logind".into())
                .spawn(move || run(command_receiver, events))?;
            Ok(Self { commands })
        }

        pub(super) fn send(&self, command: Command) {
            if self.commands.send(command).is_err() {
                warn!("the logind client is gone; ignoring the request");
            }
        }
    }

    fn run(commands: mpsc::UnboundedReceiver<Command>, events: channel::Sender<Event>) {
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(error) => {
                warn!(%error, "failed to start the logind client's runtime");
                return;
            }
        };
        runtime.block_on(async move {
            if let Err(error) = serve(commands, events).await {
                warn!(%error, "lost the connection to logind; the lid stays with logind");
            }
        });
    }

    async fn serve(
        mut commands: mpsc::UnboundedReceiver<Command>,
        events: channel::Sender<Event>,
    ) -> zbus::Result<()> {
        let connection = zbus::Connection::system().await?;
        let manager = ManagerProxy::new(&connection).await?;

        let mut sleeps = manager.receive_prepare_for_sleep().await?;
        let sleep_events = events.clone();
        tokio::spawn(async move {
            while let Some(signal) = poll_fn(|cx| Pin::new(&mut sleeps).poll_next(cx)).await {
                match signal.args() {
                    Ok(args) => {
                        let _ = sleep_events.send(Event::PrepareForSleep(args.start));
                    }
                    Err(error) => debug!(%error, "unreadable PrepareForSleep signal"),
                }
            }
        });

        // logind's own defaults stand in for a setting it does not answer: on
        // external power the lid does what it does on battery, and docked it
        // does nothing.
        let lid = setting("HandleLidSwitch", manager.handle_lid_switch().await)
            .unwrap_or_else(|| "suspend".to_owned());
        let external_power = setting(
            "HandleLidSwitchExternalPower",
            manager.handle_lid_switch_external_power().await,
        )
        .unwrap_or_else(|| lid.clone());
        let docked = setting(
            "HandleLidSwitchDocked",
            manager.handle_lid_switch_docked().await,
        )
        .unwrap_or_else(|| "ignore".to_owned());
        let _ = events.send(Event::LidActions(LogindLidActions {
            lid,
            external_power,
            docked,
        }));

        // Dropping the descriptor gives the lid back to logind.
        let mut lid_inhibitor: Option<OwnedFd> = None;
        while let Some(command) = commands.recv().await {
            match command {
                Command::HoldLid(true) => {
                    if lid_inhibitor.is_some() {
                        continue;
                    }
                    match manager
                        .inhibit(
                            "handle-lid-switch",
                            "ShojiWM",
                            "The ShojiWM config handles the lid",
                            "block",
                        )
                        .await
                    {
                        Ok(fd) => {
                            info!("holding the lid switch; logind leaves it to ShojiWM");
                            lid_inhibitor = Some(fd.into());
                        }
                        Err(error) => warn!(%error, "logind refused the lid switch"),
                    }
                }
                Command::HoldLid(false) => {
                    if lid_inhibitor.take().is_some() {
                        info!("gave the lid switch back to logind");
                    }
                }
                Command::Perform(action) => {
                    if let Err(error) = perform(&manager, action).await {
                        warn!(action = action.name(), %error, "logind refused the power action");
                    }
                }
            }
        }
        Ok(())
    }

    fn setting(name: &str, value: zbus::Result<String>) -> Option<String> {
        match value {
            Ok(value) if !value.is_empty() => Some(value),
            Ok(_) => None,
            Err(error) => {
                debug!(%error, name, "unreadable logind setting; assuming its default");
                None
            }
        }
    }

    async fn perform(manager: &ManagerProxy<'_>, action: PowerAction) -> zbus::Result<()> {
        // Not interactive: there is nobody to answer a polkit prompt.
        match action {
            PowerAction::Suspend => manager.suspend(false).await,
            PowerAction::Hibernate => manager.hibernate(false).await,
            PowerAction::HybridSleep => manager.hybrid_sleep(false).await,
            PowerAction::SuspendThenHibernate => manager.suspend_then_hibernate(false).await,
            PowerAction::Sleep => match manager.sleep(0).await {
                Err(zbus::Error::MethodError(name, ..))
                    if name.as_str() == "org.freedesktop.DBus.Error.UnknownMethod" =>
                {
                    // logind older than systemd 256 has no `Sleep`.
                    manager.suspend(false).await
                }
                result => result,
            },
            PowerAction::Lock => manager.lock_sessions().await,
            PowerAction::PowerOff => manager.power_off(false).await,
            PowerAction::Reboot => manager.reboot(false).await,
            PowerAction::Halt => manager.halt(false).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn supply(kind: &str, online: Option<bool>, scope: Option<&str>) -> PowerSupply {
        PowerSupply {
            kind: kind.into(),
            online,
            scope: scope.map(Into::into),
        }
    }

    #[test]
    fn an_online_adapter_is_external_power() {
        assert!(on_external_power(&[
            supply("Battery", None, None),
            supply("Mains", Some(true), None),
        ]));
        assert!(on_external_power(&[
            supply("Mains", Some(false), None),
            supply("USB", Some(true), None),
        ]));
    }

    #[test]
    fn offline_adapters_are_battery_power() {
        assert!(!on_external_power(&[
            supply("Battery", None, None),
            supply("Mains", Some(false), None),
            supply("USB", Some(false), None),
        ]));
    }

    #[test]
    fn no_adapter_is_external_power() {
        assert!(on_external_power(&[]));
        assert!(on_external_power(&[supply("Battery", None, None)]));
    }

    #[test]
    fn peripheral_supplies_do_not_count() {
        assert!(!on_external_power(&[
            supply("Mains", Some(false), None),
            supply("USB", Some(true), Some("Device")),
        ]));
    }

    #[test]
    fn actions_round_trip_through_their_logind_names() {
        for action in [
            PowerAction::Suspend,
            PowerAction::Hibernate,
            PowerAction::HybridSleep,
            PowerAction::SuspendThenHibernate,
            PowerAction::Sleep,
            PowerAction::Lock,
            PowerAction::PowerOff,
            PowerAction::Reboot,
            PowerAction::Halt,
        ] {
            assert_eq!(PowerAction::parse(action.name()), Some(action));
        }
        assert_eq!(PowerAction::parse("ignore"), None);
        assert_eq!(PowerAction::parse("kexec"), None);
    }
}
