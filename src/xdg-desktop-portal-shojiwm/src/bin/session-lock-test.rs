//! Debug helper: lock the session via ext-session-lock-v1 for a few seconds
//! and then unlock, the way swaylock/hyprlock do (minus the password).
//!
//!   WAYLAND_DISPLAY=wayland-2 session-lock-test [seconds]
//!
//! One lock surface (a blank shm buffer) is created per wl_output so the
//! compositor goes through its normal lock-surface focus path.

use std::os::fd::AsFd;
use std::time::{Duration, Instant};

use wayland_client::protocol::{
    wl_buffer, wl_compositor, wl_output, wl_registry, wl_shm, wl_shm_pool, wl_surface,
};
use wayland_client::{Connection, Dispatch, QueueHandle, delegate_noop};
use wayland_protocols::ext::session_lock::v1::client::{
    ext_session_lock_manager_v1::ExtSessionLockManagerV1,
    ext_session_lock_surface_v1::{self, ExtSessionLockSurfaceV1},
    ext_session_lock_v1::{self, ExtSessionLockV1},
};

#[derive(Default)]
struct App {
    compositor: Option<wl_compositor::WlCompositor>,
    shm: Option<wl_shm::WlShm>,
    manager: Option<ExtSessionLockManagerV1>,
    outputs: Vec<wl_output::WlOutput>,
    locked: bool,
    finished: bool,
    lock_surfaces: Vec<(ExtSessionLockSurfaceV1, wl_surface::WlSurface)>,
    start: Option<Instant>,
}

impl App {
    fn stamp(&self) -> String {
        let elapsed = self.start.map_or(0.0, |start| start.elapsed().as_secs_f64());
        format!("[{elapsed:7.3}s]")
    }
}

impl Dispatch<wl_registry::WlRegistry, ()> for App {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            match interface.as_str() {
                "wl_compositor" => {
                    state.compositor = Some(registry.bind::<wl_compositor::WlCompositor, _, _>(
                        name,
                        version.min(4),
                        qh,
                        (),
                    ));
                }
                "wl_shm" => {
                    state.shm = Some(registry.bind::<wl_shm::WlShm, _, _>(name, 1, qh, ()));
                }
                "wl_output" => {
                    state
                        .outputs
                        .push(registry.bind::<wl_output::WlOutput, _, _>(name, version.min(4), qh, ()));
                }
                "ext_session_lock_manager_v1" => {
                    state.manager = Some(registry.bind::<ExtSessionLockManagerV1, _, _>(
                        name,
                        1,
                        qh,
                        (),
                    ));
                }
                _ => {}
            }
        }
    }
}

delegate_noop!(App: ignore wl_compositor::WlCompositor);
delegate_noop!(App: ignore wl_shm::WlShm);
delegate_noop!(App: ignore wl_shm_pool::WlShmPool);
delegate_noop!(App: ignore wl_buffer::WlBuffer);
delegate_noop!(App: ignore wl_surface::WlSurface);
delegate_noop!(App: ignore wl_output::WlOutput);
delegate_noop!(App: ignore ExtSessionLockManagerV1);

impl Dispatch<ExtSessionLockV1, ()> for App {
    fn event(
        state: &mut Self,
        _: &ExtSessionLockV1,
        event: ext_session_lock_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            ext_session_lock_v1::Event::Locked => {
                println!("{} locked", state.stamp());
                state.locked = true;
            }
            ext_session_lock_v1::Event::Finished => {
                println!("{} finished (compositor refused or ended the lock)", state.stamp());
                state.finished = true;
            }
            _ => {}
        }
    }
}

impl Dispatch<ExtSessionLockSurfaceV1, wl_surface::WlSurface> for App {
    fn event(
        state: &mut Self,
        lock_surface: &ExtSessionLockSurfaceV1,
        event: ext_session_lock_surface_v1::Event,
        surface: &wl_surface::WlSurface,
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let ext_session_lock_surface_v1::Event::Configure {
            serial,
            width,
            height,
        } = event
        {
            println!("{} lock surface configure {width}x{height}", state.stamp());
            lock_surface.ack_configure(serial);
            let shm = state.shm.as_ref().expect("wl_shm");
            let stride = width as i32 * 4;
            let size = stride * height as i32;
            let fd = rustix::fs::memfd_create("shoji-lock-test", rustix::fs::MemfdFlags::CLOEXEC)
                .expect("memfd_create");
            rustix::fs::ftruncate(&fd, size as u64).expect("ftruncate");
            let pool = shm.create_pool(fd.as_fd(), size, qh, ());
            let buffer = pool.create_buffer(
                0,
                width as i32,
                height as i32,
                stride,
                wl_shm::Format::Argb8888,
                qh,
                (),
            );
            surface.attach(Some(&buffer), 0, 0);
            surface.damage_buffer(0, 0, width as i32, height as i32);
            surface.commit();
        }
    }
}

fn main() {
    let seconds: u64 = std::env::args()
        .nth(1)
        .and_then(|value| value.parse().ok())
        .unwrap_or(3);

    let conn = Connection::connect_to_env().expect("connect");
    let mut queue = conn.new_event_queue::<App>();
    let qh = queue.handle();
    let _registry = conn.display().get_registry(&qh, ());
    let mut app = App {
        start: Some(Instant::now()),
        ..App::default()
    };
    queue.roundtrip(&mut app).expect("roundtrip registry");
    queue.roundtrip(&mut app).expect("roundtrip globals");

    let manager = app.manager.clone().expect("no ext_session_lock_manager_v1");
    let compositor = app.compositor.clone().expect("wl_compositor");
    let lock = manager.lock(&qh, ());
    println!("{} lock requested for {} outputs", app.stamp(), app.outputs.len());
    for output in app.outputs.clone() {
        let surface = compositor.create_surface(&qh, ());
        let lock_surface = lock.get_lock_surface(&surface, &output, &qh, surface.clone());
        app.lock_surfaces.push((lock_surface, surface));
    }

    let deadline = Instant::now() + Duration::from_secs(seconds);
    while Instant::now() < deadline && !app.finished {
        queue.flush().expect("flush");
        if let Some(guard) = conn.prepare_read() {
            let _ = guard.read_without_dispatch();
        }
        queue.dispatch_pending(&mut app).expect("dispatch");
        std::thread::sleep(Duration::from_millis(20));
    }
    if app.finished {
        std::process::exit(1);
    }
    println!("{} unlock_and_destroy", app.stamp());
    lock.unlock_and_destroy();
    queue.roundtrip(&mut app).expect("roundtrip unlock");
    println!("{} done", app.stamp());
}
