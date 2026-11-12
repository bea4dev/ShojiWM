//! A compositor built from a Rust config, with no backend, and a real
//! Wayland client connected to its listening socket, both driven from the
//! test's thread.

#![allow(dead_code)]

use std::{
    fs::File,
    os::{fd::AsFd, unix::net::UnixStream},
    path::PathBuf,
    time::Duration,
};

use shojiwm_lib::state::ShojiWM;
use shojiwm_rs::{ConfigBuilder, RuntimeBoot, cli::CommonArgs};
use smithay::{
    output::{Mode, Output, PhysicalProperties, Subpixel},
    reexports::{calloop::EventLoop, wayland_server::Display},
};
use wayland_client::{
    Connection, Dispatch, EventQueue, Proxy, QueueHandle,
    protocol::{
        wl_buffer, wl_compositor, wl_output, wl_registry, wl_seat, wl_shm, wl_shm_pool, wl_surface,
    },
};
use wayland_protocols::xdg::shell::client::{xdg_surface, xdg_toplevel, xdg_wm_base};
use wayland_protocols_wlr::layer_shell::v1::client::{zwlr_layer_shell_v1, zwlr_layer_surface_v1};

pub struct Session {
    pub event_loop: EventLoop<'static, ShojiWM>,
    pub state: ShojiWM,
    pub connection: Connection,
    pub queue: EventQueue<Client>,
    pub client: Client,
    pub registry: wl_registry::WlRegistry,
    /// Backing files of shm buffers; their fds must outlive the requests.
    files: Vec<File>,
    _runtime_dir: PathBuf,
}

impl Session {
    pub fn new(setup: fn()) -> Self {
        // One directory per test binary; tests in a binary run on threads of
        // one process, so XDG_RUNTIME_DIR is shared and set once.
        let runtime_dir =
            std::env::temp_dir().join(format!("shoji-session-{}", std::process::id()));
        std::fs::create_dir_all(&runtime_dir).unwrap();
        // SAFETY: every test sets the same value before reading it.
        unsafe { std::env::set_var("XDG_RUNTIME_DIR", &runtime_dir) };

        let mut event_loop: EventLoop<ShojiWM> = EventLoop::try_new().unwrap();
        let display: Display<ShojiWM> = Display::new().unwrap();
        let args = CommonArgs::parse(&[], &[]);
        let boot = RuntimeBoot::new(Box::new(ConfigBuilder::new(setup)), &args);
        let mut state = ShojiWM::new(&mut event_loop, display, &boot);
        let output = Output::new(
            "TEST-1".into(),
            PhysicalProperties {
                size: (0, 0).into(),
                subpixel: Subpixel::Unknown,
                make: "test".into(),
                model: "test".into(),
                serial_number: String::new(),
            },
        );
        let mode = Mode {
            size: (1024, 768).into(),
            refresh: 60_000,
        };
        output.change_current_state(Some(mode), None, None, Some((0, 0).into()));
        output.set_preferred(mode);
        state.create_output_global(&output);
        state.space.map_output(&output, (0, 0));
        state.enable_initial_decoration_runtime();

        let socket = UnixStream::connect(runtime_dir.join(&state.socket_name)).unwrap();
        let connection = Connection::from_socket(socket).unwrap();
        let queue = connection.new_event_queue();
        let registry = connection.display().get_registry(&queue.handle(), ());
        let mut session = Self {
            event_loop,
            state,
            connection,
            queue,
            client: Client::default(),
            registry,
            files: Vec::new(),
            _runtime_dir: runtime_dir,
        };
        session.roundtrip();
        session
    }

    /// A few passes: the listener accepts the client on one, its requests are
    /// read on the next.
    pub fn roundtrip(&mut self) {
        for _ in 0..5 {
            self.connection.flush().unwrap();
            self.event_loop
                .dispatch(Some(Duration::from_millis(5)), &mut self.state)
                .unwrap();
            self.state.display_handle.flush_clients().unwrap();
            if let Some(guard) = self.queue.prepare_read() {
                let _ = guard.read();
            }
            self.queue.dispatch_pending(&mut self.client).unwrap();
        }
    }

    pub fn bind<I>(&self, interface: &str, version: u32) -> I
    where
        I: Proxy + 'static,
        Client: Dispatch<I, ()>,
    {
        self.registry.bind::<I, _, _>(
            self.client.global(interface),
            version,
            &self.queue.handle(),
            (),
        )
    }

    /// Maps an xdg toplevel. ShojiWM maps it into the space as soon as the
    /// role exists, before any buffer.
    pub fn map_toplevel(&mut self) -> wl_surface::WlSurface {
        let qh = self.queue.handle();
        let compositor: wl_compositor::WlCompositor = self.bind("wl_compositor", 1);
        let shell: xdg_wm_base::XdgWmBase = self.bind("xdg_wm_base", 1);
        let surface = compositor.create_surface(&qh, ());
        let xdg = shell.get_xdg_surface(&surface, &qh, ());
        xdg.get_toplevel(&qh, ());
        surface.commit();
        self.roundtrip();
        surface
    }

    pub fn bind_global<I>(&self, name: u32, version: u32) -> I
    where
        I: Proxy + 'static,
        Client: Dispatch<I, ()>,
    {
        self.registry
            .bind::<I, _, _>(name, version, &self.queue.handle(), ())
    }

    /// Maps an xdg toplevel with `app_id` and a `width`x`height` buffer, so
    /// the pointer can land on it.
    pub fn map_toplevel_with_buffer(
        &mut self,
        app_id: &str,
        width: i32,
        height: i32,
    ) -> wl_surface::WlSurface {
        let qh = self.queue.handle();
        let compositor: wl_compositor::WlCompositor = self.bind("wl_compositor", 1);
        let shell: xdg_wm_base::XdgWmBase = self.bind("xdg_wm_base", 1);
        let shm: wl_shm::WlShm = self.bind("wl_shm", 1);
        let surface = compositor.create_surface(&qh, ());
        let xdg = shell.get_xdg_surface(&surface, &qh, ());
        let toplevel = xdg.get_toplevel(&qh, ());
        toplevel.set_app_id(app_id.into());
        surface.commit();
        self.roundtrip();

        let stride = width * 4;
        let file = tempfile("shm");
        file.set_len((stride * height) as u64).unwrap();
        let pool = shm.create_pool(file.as_fd(), stride * height, &qh, ());
        let buffer =
            pool.create_buffer(0, width, height, stride, wl_shm::Format::Argb8888, &qh, ());
        self.files.push(file);
        surface.attach(Some(&buffer), 0, 0);
        surface.damage(0, 0, width, height);
        surface.commit();
        self.roundtrip();
        surface
    }

    /// Maps a layer surface across the top of the first output.
    pub fn map_layer(&mut self, namespace: &str) -> wl_surface::WlSurface {
        self.map_layer_on(namespace, None)
    }

    /// Maps a layer surface across the top of `output`, or of the first
    /// output.
    pub fn map_layer_on(
        &mut self,
        namespace: &str,
        output: Option<&wl_output::WlOutput>,
    ) -> wl_surface::WlSurface {
        let qh = self.queue.handle();
        let compositor: wl_compositor::WlCompositor = self.bind("wl_compositor", 1);
        let shell: zwlr_layer_shell_v1::ZwlrLayerShellV1 = self.bind("zwlr_layer_shell_v1", 1);
        let surface = compositor.create_surface(&qh, ());
        let layer = shell.get_layer_surface(
            &surface,
            output,
            zwlr_layer_shell_v1::Layer::Top,
            namespace.into(),
            &qh,
            (),
        );
        layer.set_anchor(
            zwlr_layer_surface_v1::Anchor::Top
                | zwlr_layer_surface_v1::Anchor::Left
                | zwlr_layer_surface_v1::Anchor::Right,
        );
        layer.set_size(0, 40);
        surface.commit();
        self.roundtrip();
        surface
    }
}

#[derive(Default)]
pub struct Client {
    globals: Vec<(String, u32)>,
    /// Layer surfaces the compositor sent `closed`.
    pub closed_layers: usize,
}

impl Client {
    /// Names of the globals of `interface` advertised and not removed.
    pub fn globals_of(&self, interface: &str) -> Vec<u32> {
        self.globals
            .iter()
            .filter(|(candidate, _)| candidate == interface)
            .map(|(_, name)| *name)
            .collect()
    }

    fn global(&self, interface: &str) -> u32 {
        self.globals
            .iter()
            .find(|(name, _)| name == interface)
            .unwrap_or_else(|| panic!("{interface} not advertised"))
            .1
    }
}

impl Dispatch<wl_registry::WlRegistry, ()> for Client {
    fn event(
        client: &mut Self,
        _: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_registry::Event::Global {
                name, interface, ..
            } => client.globals.push((interface, name)),
            wl_registry::Event::GlobalRemove { name } => {
                client.globals.retain(|(_, global)| *global != name);
            }
            _ => {}
        }
    }
}

/// An unlinked file for shared memory or a keymap.
pub fn tempfile(purpose: &str) -> File {
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path =
        std::env::temp_dir().join(format!("shoji-test-{purpose}-{}-{n}", std::process::id()));
    let file = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path)
        .unwrap();
    std::fs::remove_file(&path).unwrap();
    file
}

impl Dispatch<xdg_surface::XdgSurface, ()> for Client {
    fn event(
        _: &mut Self,
        surface: &xdg_surface::XdgSurface,
        event: xdg_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            surface.ack_configure(serial);
        }
    }
}

impl Dispatch<xdg_wm_base::XdgWmBase, ()> for Client {
    fn event(
        _: &mut Self,
        shell: &xdg_wm_base::XdgWmBase,
        event: xdg_wm_base::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            shell.pong(serial);
        }
    }
}

impl Dispatch<zwlr_layer_surface_v1::ZwlrLayerSurfaceV1, ()> for Client {
    fn event(
        client: &mut Self,
        layer: &zwlr_layer_surface_v1::ZwlrLayerSurfaceV1,
        event: zwlr_layer_surface_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_layer_surface_v1::Event::Configure { serial, .. } => layer.ack_configure(serial),
            zwlr_layer_surface_v1::Event::Closed => client.closed_layers += 1,
            _ => {}
        }
    }
}

macro_rules! ignore_events {
    ($($proxy:ty),*) => {$(
        impl Dispatch<$proxy, ()> for Client {
            fn event(_: &mut Self, _: &$proxy, _: <$proxy as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
        }
    )*};
}

ignore_events!(
    wl_compositor::WlCompositor,
    wl_surface::WlSurface,
    wl_shm::WlShm,
    wl_shm_pool::WlShmPool,
    wl_buffer::WlBuffer,
    xdg_toplevel::XdgToplevel,
    zwlr_layer_shell_v1::ZwlrLayerShellV1,
    wl_seat::WlSeat,
    wl_output::WlOutput
);
