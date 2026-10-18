//! Holds the pointer still while a control is dragged, and reports how far the mouse moved
//! instead, so a drag can run past the edges of the window and of the screen. On Wayland the
//! pointer is locked to the settings window through the compositor's pointer constraints, on
//! GPUI's own connection. Where that is not possible the caller follows the pointer itself.

use gpui::{Point, Window};

pub(crate) struct Grab {
    #[cfg(target_os = "linux")]
    lock: Option<wayland::Lock>,
}

impl Grab {
    /// Locks the pointer where it is, over `window`. It stays locked until the grab is dropped.
    pub fn start(window: &Window) -> Self {
        #[cfg(target_os = "linux")]
        {
            let lock = wayland::Lock::new(window)
                .inspect_err(|error| log::info!("grab: the pointer stays free: {error:#}"))
                .ok();
            Self { lock }
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = window;
            Self {}
        }
    }

    /// How far the mouse moved since the last call, in logical pixels, once the compositor has
    /// locked the pointer. `None` until then, or when it never will, and the caller follows the
    /// pointer's position instead.
    pub fn moved(&mut self) -> Option<Point<f32>> {
        #[cfg(target_os = "linux")]
        if let Some(lock) = &mut self.lock {
            return lock.moved();
        }
        None
    }
}

#[cfg(target_os = "linux")]
mod wayland {
    use anyhow::{Context as _, Result, anyhow, bail};
    use gpui::{Point, Window, point};
    use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};
    use wayland_client::backend::{Backend, ObjectId};
    use wayland_client::globals::{GlobalListContents, registry_queue_init};
    use wayland_client::protocol::wl_pointer::WlPointer;
    use wayland_client::protocol::wl_registry::WlRegistry;
    use wayland_client::protocol::wl_seat::WlSeat;
    use wayland_client::protocol::wl_surface::WlSurface;
    use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle};
    use wayland_protocols::wp::pointer_constraints::zv1::client::zwp_locked_pointer_v1::{
        self, ZwpLockedPointerV1,
    };
    use wayland_protocols::wp::pointer_constraints::zv1::client::zwp_pointer_constraints_v1::{
        Lifetime, ZwpPointerConstraintsV1,
    };
    use wayland_protocols::wp::relative_pointer::zv1::client::zwp_relative_pointer_manager_v1::ZwpRelativePointerManagerV1;
    use wayland_protocols::wp::relative_pointer::zv1::client::zwp_relative_pointer_v1::{
        self, ZwpRelativePointerV1,
    };

    #[derive(Default)]
    struct State {
        locked: bool,
        moved: (f64, f64),
    }

    /// A pointer lock on the settings window, made on a queue of its own over GPUI's connection.
    /// GPUI reads the socket, which files the lock's events on this queue, and the drag takes
    /// them each frame.
    pub(super) struct Lock {
        connection: Connection,
        queue: EventQueue<State>,
        state: State,
        seat: WlSeat,
        pointer: WlPointer,
        relative: ZwpRelativePointerV1,
        locked: ZwpLockedPointerV1,
    }

    impl Lock {
        pub fn new(window: &Window) -> Result<Self> {
            // GPUI's own `window_handle` names the window inside GPUI, not the platform window.
            let display =
                HasDisplayHandle::display_handle(window).map_err(|error| anyhow!("{error}"))?;
            let surface =
                HasWindowHandle::window_handle(window).map_err(|error| anyhow!("{error}"))?;
            let (RawDisplayHandle::Wayland(display), RawWindowHandle::Wayland(surface)) =
                (display.as_raw(), surface.as_raw())
            else {
                bail!("not a Wayland window");
            };
            // SAFETY: the display is GPUI's, which stays connected while its windows are open, and
            // a foreign display is never disconnected when this connection goes.
            let backend = unsafe { Backend::from_foreign_display(display.display.as_ptr().cast()) };
            let connection = Connection::from_backend(backend);
            // SAFETY: the pointer is the window's live wl_surface on that display.
            let surface = unsafe {
                ObjectId::from_ptr(WlSurface::interface(), surface.surface.as_ptr().cast())
            }
            .context("reading the window's surface")?;
            let surface = WlSurface::from_id(&connection, surface)?;

            let (globals, queue) = registry_queue_init::<State>(&connection)?;
            let handle = queue.handle();
            let seat: WlSeat = globals.bind(&handle, 1..=5, ())?;
            let constraints: ZwpPointerConstraintsV1 = globals
                .bind(&handle, 1..=1, ())
                .context("no pointer constraints")?;
            let relatives: ZwpRelativePointerManagerV1 = globals
                .bind(&handle, 1..=1, ())
                .context("no relative pointer")?;
            let pointer = seat.get_pointer(&handle, ());
            let relative = relatives.get_relative_pointer(&pointer, &handle, ());
            let locked =
                constraints.lock_pointer(&surface, &pointer, None, Lifetime::Oneshot, &handle, ());
            constraints.destroy();
            relatives.destroy();
            connection.flush()?;
            Ok(Self {
                connection,
                queue,
                state: State::default(),
                seat,
                pointer,
                relative,
                locked,
            })
        }

        pub fn moved(&mut self) -> Option<Point<f32>> {
            if let Err(error) = self.queue.dispatch_pending(&mut self.state) {
                log::warn!("grab: {error}");
            }
            if !self.state.locked {
                return None;
            }
            let (x, y) = std::mem::take(&mut self.state.moved);
            Some(point(x as f32, y as f32))
        }
    }

    impl Drop for Lock {
        fn drop(&mut self) {
            self.locked.destroy();
            self.relative.destroy();
            if self.pointer.version() >= 3 {
                self.pointer.release();
            }
            if self.seat.version() >= 5 {
                self.seat.release();
            }
            self.connection.flush().ok();
        }
    }

    impl Dispatch<ZwpLockedPointerV1, ()> for State {
        fn event(
            state: &mut Self,
            _: &ZwpLockedPointerV1,
            event: zwp_locked_pointer_v1::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            match event {
                zwp_locked_pointer_v1::Event::Locked => state.locked = true,
                zwp_locked_pointer_v1::Event::Unlocked => state.locked = false,
                _ => {}
            }
        }
    }

    impl Dispatch<ZwpRelativePointerV1, ()> for State {
        fn event(
            state: &mut Self,
            _: &ZwpRelativePointerV1,
            event: zwp_relative_pointer_v1::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            if let zwp_relative_pointer_v1::Event::RelativeMotion { dx, dy, .. } = event {
                state.moved.0 += dx;
                state.moved.1 += dy;
            }
        }
    }

    impl Dispatch<WlRegistry, GlobalListContents> for State {
        fn event(
            _: &mut Self,
            _: &WlRegistry,
            _: <WlRegistry as Proxy>::Event,
            _: &GlobalListContents,
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
        }
    }

    wayland_client::delegate_noop!(State: ignore WlSeat);
    wayland_client::delegate_noop!(State: ignore WlPointer);
    wayland_client::delegate_noop!(State: ZwpPointerConstraintsV1);
    wayland_client::delegate_noop!(State: ZwpRelativePointerManagerV1);
}
