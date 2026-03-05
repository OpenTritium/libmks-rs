//! <https://wayland.app/protocols/pointer-constraints-unstable-v1>
use crate::{display::input_daemon::InputCommand, mks_debug, mks_error, mks_info};
use gdk4_wayland::{
    WaylandDisplay,
    gdk::Rectangle,
    wayland_client::{
        Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum,
        protocol::{
            wl_compositor::{self, WlCompositor},
            wl_pointer::{self, WlPointer},
            wl_region::{self, WlRegion},
            wl_registry::{self, WlRegistry},
            wl_seat::{self, Capability, WlSeat},
            wl_surface::WlSurface,
        },
    },
};
use kanal::Sender;
use std::{
    cell::RefCell,
    mem,
    ops::DerefMut,
    os::unix::io::{AsFd, RawFd},
    rc::Rc,
};
// use wayland_client::{
//     Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum,
//     protocol::{
//         wl_compositor::{self, WlCompositor},
//         wl_pointer::{self, WlPointer},
//         wl_region::{self, WlRegion},
//         wl_registry::{self, WlRegistry},
//         wl_seat::{self, Capability, WlSeat},
//         wl_surface::WlSurface,
//     },
// };
use wayland_protocols::wp::{
    pointer_constraints::zv1::client::{
        zwp_confined_pointer_v1::{self, ZwpConfinedPointerV1},
        zwp_locked_pointer_v1::{self, ZwpLockedPointerV1},
        zwp_pointer_constraints_v1::{self, Lifetime, ZwpPointerConstraintsV1},
    },
    relative_pointer::zv1::client::{
        zwp_relative_pointer_manager_v1::{self, ZwpRelativePointerManagerV1},
        zwp_relative_pointer_v1::{self, ZwpRelativePointerV1},
    },
};

const LOG_TARGET: &str = "mks.display.wayland";

#[derive(Default)]
pub struct WaylandState {
    pointer_constraints: Option<ZwpPointerConstraintsV1>,
    relative_pointer_manager: Option<ZwpRelativePointerManagerV1>,
    compositor: Option<WlCompositor>,
    seat: Option<WlSeat>,
    pointer: Option<WlPointer>,
    pointer_capture: PointerCapture,
    input_tx: Option<Sender<InputCommand>>,
}

#[derive(Default)]
enum PointerCapture {
    #[default]
    None,
    Confined(ZwpConfinedPointerV1),
    LockedRelative {
        locked: ZwpLockedPointerV1,
        relative: ZwpRelativePointerV1,
    },
}

pub struct WaylandConfine {
    conn: Connection,
    event_queue: RefCell<EventQueue<WaylandState>>,
    qh: QueueHandle<WaylandState>,
    state: Rc<RefCell<WaylandState>>,
}

impl WaylandConfine {
    pub fn from_gdk(gdk_display: &WaylandDisplay, input_tx: Sender<InputCommand>) -> Self {
        mks_info!("Initializing Wayland pointer confinement through the GDK bridge");
        let wl_display = gdk_display.wl_display().expect("Failed to get WlDisplay");
        let backend = wl_display.backend().upgrade().expect("Wayland connection is dead");
        let conn = Connection::from_backend(backend);
        let mut event_queue = conn.new_event_queue();
        let qh = event_queue.handle();
        let state = Rc::new(RefCell::new(WaylandState::default()));
        state.borrow_mut().input_tx = Some(input_tx);
        let display_proxy = conn.display();
        let _registry = display_proxy.get_registry(&qh, ());
        // Wait for globals so we can bind seat and pointer-constraints interfaces.
        if let Err(e) = event_queue.roundtrip(&mut *state.borrow_mut()) {
            mks_error!(error:? = e; "Wayland registry roundtrip failed");
        }
        // Wait for seat capabilities so we can request wl_pointer.
        if let Err(e) = event_queue.roundtrip(&mut *state.borrow_mut()) {
            mks_error!(error:? = e; "Wayland seat capability roundtrip failed");
        }
        Self { conn, event_queue: RefCell::new(event_queue), qh, state }
    }

    /// Constrains the pointer to `rect` on the target surface.
    ///
    /// Returns `true` only when pointer capture has been established successfully.
    ///
    /// When `prefer_relative` is true, this requires native relative-pointer protocol.
    /// Otherwise use region confinement for absolute guest mouse mode.
    pub fn confine_pointer(&self, surface: &WlSurface, rect: &Rectangle, prefer_relative: bool) -> bool {
        let mut state = self.state.borrow_mut();
        let Some(constraints) = state.pointer_constraints.as_ref() else {
            mks_error!("Wayland pointer-constraints protocol unavailable; cannot confine pointer");
            return false;
        };
        let Some(pointer) = state.pointer.as_ref() else {
            mks_error!("Wayland pointer unavailable; cannot confine pointer");
            return false;
        };
        if !matches!(state.pointer_capture, PointerCapture::None) {
            mks_error!("Pointer capture already active; ignoring duplicate confine request");
            return false;
        }

        if prefer_relative {
            let Some(relative_manager) = state.relative_pointer_manager.as_ref() else {
                mks_error!("Relative pointer protocol unavailable; cannot enable relative capture");
                return false;
            };
            let relative = relative_manager.get_relative_pointer(pointer, &self.qh, ());
            let locked = constraints.lock_pointer(surface, pointer, None, Lifetime::Persistent, &self.qh, ());
            state.pointer_capture = PointerCapture::LockedRelative { locked, relative };
            mks_info!("Pointer locked with native relative motion enabled");
        } else {
            // Absolute guest mode path.
            let Some(compositor) = state.compositor.as_ref() else {
                mks_error!("Wayland compositor unavailable; cannot confine pointer in absolute mode");
                return false;
            };
            let region = compositor.create_region(&self.qh, ());
            region.add(rect.x(), rect.y(), rect.width(), rect.height());
            let confined =
                constraints.confine_pointer(surface, pointer, Some(&region), Lifetime::Persistent, &self.qh, ());
            region.destroy();
            state.pointer_capture = PointerCapture::Confined(confined);
            mks_info!("Pointer confined to region for absolute guest mouse mode");
        }
        drop(state);
        if let Err(e) = self.conn.flush() {
            mks_error!(error:? = e; "Failed to flush Wayland connection");
        }
        true
    }

    pub fn unconfine(&self) {
        let mut state = self.state.borrow_mut();
        let mut released = false;
        match mem::take(&mut state.pointer_capture) {
            PointerCapture::LockedRelative { locked, relative } => {
                relative.destroy();
                locked.destroy();
                mks_info!("Released pointer lock");
                released = true;
            }
            PointerCapture::Confined(confined) => {
                confined.destroy();
                mks_info!("Released pointer confinement");
                released = true;
            }
            PointerCapture::None => {}
        }

        if released {
            if let Err(e) = self.conn.flush() {
                mks_error!(error:? = e; "Failed to flush Wayland connection");
            }
        } else {
            mks_error!("Pointer capture is not active; nothing to release");
        }
    }

    #[inline]
    pub fn dispatch_pending(&self) {
        let mut state = self.state.borrow_mut();
        if let Err(e) = self.event_queue.borrow_mut().dispatch_pending(state.deref_mut()) {
            mks_error!(error:? = e; "Failed to dispatch pending Wayland events");
        }
        if let Err(e) = self.conn.flush() {
            mks_error!(error:? = e; "Failed to flush Wayland connection");
        }
    }

    #[inline]
    pub fn get_conn_raw_fd(&self) -> RawFd {
        use std::os::unix::io::AsRawFd;
        self.conn.as_fd().as_raw_fd()
    }
}

impl Dispatch<WlRegistry, ()> for WaylandState {
    #[inline]
    fn event(
        state: &mut Self, registry: &WlRegistry, event: wl_registry::Event, _: &(), _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global { name, interface, version: _ } = event {
            match interface.as_str() {
                "zwp_pointer_constraints_v1" => {
                    state.pointer_constraints = Some(registry.bind(name, 1, qh, ()));
                }
                "zwp_relative_pointer_manager_v1" => {
                    state.relative_pointer_manager = Some(registry.bind(name, 1, qh, ()));
                }
                "wl_seat" => {
                    state.seat = Some(registry.bind(name, 1, qh, ()));
                }
                "wl_compositor" => {
                    state.compositor = Some(registry.bind(name, 1, qh, ()));
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<WlSeat, ()> for WaylandState {
    #[inline]
    fn event(state: &mut Self, seat: &WlSeat, event: wl_seat::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        if let wl_seat::Event::Capabilities { capabilities } = event {
            let cap = match capabilities {
                WEnum::Value(v) => v,
                WEnum::Unknown(u) => Capability::from_bits_retain(u),
            };
            if cap.contains(Capability::Pointer) && state.pointer.is_none() {
                state.pointer = Some(seat.get_pointer(qh, ()));
            }
        }
    }
}

impl Dispatch<ZwpConfinedPointerV1, ()> for WaylandState {
    fn event(
        _: &mut Self, _: &ZwpConfinedPointerV1, event: zwp_confined_pointer_v1::Event, _: &(), _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwp_confined_pointer_v1::Event::Confined => mks_debug!("Received pointer confined event"),
            zwp_confined_pointer_v1::Event::Unconfined => {
                mks_debug!("Received pointer unconfined event")
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwpLockedPointerV1, ()> for WaylandState {
    fn event(
        _: &mut Self, _: &ZwpLockedPointerV1, event: zwp_locked_pointer_v1::Event, _: &(), _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwp_locked_pointer_v1::Event::Locked => mks_debug!("Received pointer locked event"),
            zwp_locked_pointer_v1::Event::Unlocked => mks_debug!("Received pointer unlocked event"),
            _ => {}
        }
    }
}

impl Dispatch<ZwpRelativePointerV1, ()> for WaylandState {
    fn event(
        state: &mut Self, _: &ZwpRelativePointerV1, event: zwp_relative_pointer_v1::Event, _: &(), _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwp_relative_pointer_v1::Event::RelativeMotion {
            utime_hi: _,
            utime_lo: _,
            dx,
            dy,
            dx_unaccel: _,
            dy_unaccel: _,
        } = event
        {
            #[inline]
            fn quantize_realtime_delta(delta: f64) -> i32 {
                if !delta.is_finite() || delta == 0.0 {
                    return 0;
                }
                let rounded = delta.round();
                if rounded == 0.0 {
                    // Preserve tiny non-zero deltas instead of accumulating/skipping them.
                    delta.signum() as i32
                } else {
                    rounded as i32
                }
            }
            let step_x = quantize_realtime_delta(dx);
            let step_y = quantize_realtime_delta(dy);

            if (step_x != 0 || step_y != 0)
                && let Some(tx) = state.input_tx.as_ref()
                && let Err(e) = tx.send(InputCommand::MouseRel(step_x, step_y))
            {
                mks_error!(error:? = e; "Failed to forward native relative motion; dropping event");
            }
        }
    }
}

impl Dispatch<zwp_pointer_constraints_v1::ZwpPointerConstraintsV1, ()> for WaylandState {
    fn event(
        _: &mut Self, _: &ZwpPointerConstraintsV1, _: zwp_pointer_constraints_v1::Event, _: &(), _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwpRelativePointerManagerV1, ()> for WaylandState {
    fn event(
        _: &mut Self, _: &ZwpRelativePointerManagerV1, _: zwp_relative_pointer_manager_v1::Event, _: &(),
        _: &Connection, _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_pointer::WlPointer, ()> for WaylandState {
    fn event(_: &mut Self, _: &WlPointer, _: wl_pointer::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<WlCompositor, ()> for WaylandState {
    fn event(_: &mut Self, _: &WlCompositor, _: wl_compositor::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<WlRegion, ()> for WaylandState {
    fn event(_: &mut Self, _: &WlRegion, _: wl_region::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}
