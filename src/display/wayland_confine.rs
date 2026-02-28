//! <https://wayland.app/protocols/pointer-constraints-unstable-v1>
use crate::dbus::mouse::MouseController;
use gdk4_wayland::{WaylandDisplay, gdk::Rectangle};
use log::{debug, info, warn};
use std::{
    cell::RefCell,
    mem,
    ops::DerefMut,
    os::unix::io::{AsFd, RawFd},
    rc::Rc,
};
use wayland_client::{
    Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum,
    protocol::{
        wl_compositor::{self, WlCompositor},
        wl_pointer::{self, WlPointer},
        wl_region::{self, WlRegion},
        wl_registry::{self, WlRegistry},
        wl_seat::{self, Capability, WlSeat},
        wl_surface::WlSurface,
    },
};
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

#[derive(Default)]
pub struct WaylandState {
    pointer_constraints: Option<ZwpPointerConstraintsV1>,
    relative_pointer_manager: Option<ZwpRelativePointerManagerV1>,
    compositor: Option<WlCompositor>,
    seat: Option<WlSeat>,
    pointer: Option<WlPointer>,
    pointer_capture: PointerCapture,
    mouse_ctrl: Option<MouseController>,
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
    pub fn from_gdk(gdk_display: &WaylandDisplay, mouse_ctrl: MouseController) -> Self {
        info!("Initializing WaylandConfine using GDK safe bridge");
        let wl_display = gdk_display.wl_display().expect("Failed to get WlDisplay");
        let backend = wl_display.backend().upgrade().expect("Wayland connection is dead");
        let conn = Connection::from_backend(backend);
        let mut event_queue = conn.new_event_queue();
        let qh = event_queue.handle();
        let state = Rc::new(RefCell::new(WaylandState::default()));
        state.borrow_mut().mouse_ctrl = Some(mouse_ctrl);
        let display_proxy = conn.display();
        let _registry = display_proxy.get_registry(&qh, ());
        // 等待服务器发回 Global 列表 (拿到 Seat 和 Constraints)
        if let Err(e) = event_queue.roundtrip(&mut *state.borrow_mut()) {
            warn!(error:? = e; "Roundtrip 1 failed");
        }
        // 等待 Seat 发回 Capabilities -> 触发我们去拿 Pointer
        if let Err(e) = event_queue.roundtrip(&mut *state.borrow_mut()) {
            warn!(error:? = e; "Roundtrip 2 failed");
        }
        Self { conn, event_queue: RefCell::new(event_queue), qh, state }
    }

    /// 将指针约束在一个矩形内。
    ///
    /// Returns `true` only when pointer capture has been established successfully.
    ///
    /// When `prefer_relative` is true, this requires native relative-pointer protocol.
    /// Otherwise use region confinement for absolute guest mouse mode.
    pub fn confine_pointer(&self, surface: &WlSurface, rect: &Rectangle, prefer_relative: bool) -> bool {
        let mut state = self.state.borrow_mut();
        let Some(constraints) = state.pointer_constraints.as_ref() else {
            warn!("Pointer constraints not available");
            return false;
        };
        let Some(pointer) = state.pointer.as_ref() else {
            warn!("Pointer not available");
            return false;
        };
        if !matches!(state.pointer_capture, PointerCapture::None) {
            warn!("Pointer capture already active");
            return false;
        }

        if prefer_relative {
            let Some(relative_manager) = state.relative_pointer_manager.as_ref() else {
                warn!("Relative pointer protocol unavailable; refusing relative capture fallback");
                return false;
            };
            let relative = relative_manager.get_relative_pointer(pointer, &self.qh, ());
            let locked = constraints.lock_pointer(surface, pointer, None, Lifetime::Persistent, &self.qh, ());
            state.pointer_capture = PointerCapture::LockedRelative { locked, relative };
            info!("Pointer locked with native relative motion");
        } else {
            // Absolute guest mode path.
            let Some(compositor) = state.compositor.as_ref() else {
                warn!("Compositor not available");
                return false;
            };
            let region = compositor.create_region(&self.qh, ());
            region.add(rect.x(), rect.y(), rect.width(), rect.height());
            let confined =
                constraints.confine_pointer(surface, pointer, Some(&region), Lifetime::Persistent, &self.qh, ());
            region.destroy();
            state.pointer_capture = PointerCapture::Confined(confined);
            info!("Pointer confined with region mode (absolute guest mouse)");
        }
        drop(state);
        if let Err(e) = self.conn.flush() {
            warn!(error:? = e; "Failed to flush connection");
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
                info!("Pointer lock released");
                released = true;
            }
            PointerCapture::Confined(confined) => {
                confined.destroy();
                info!("Pointer confine released");
                released = true;
            }
            PointerCapture::None => {}
        }

        if released {
            if let Err(e) = self.conn.flush() {
                warn!(error:? = e; "Failed to flush connection");
            }
        } else {
            warn!("Cannot unconfine a pointer that is not confined");
        }
    }

    #[inline]
    pub fn dispatch_pending(&self) {
        let mut state = self.state.borrow_mut();
        if let Err(e) = self.event_queue.borrow_mut().dispatch_pending(state.deref_mut()) {
            warn!(error:? = e; "Failed to dispatch pending events");
        }
        if let Err(e) = self.conn.flush() {
            warn!(error:? = e; "Failed to flush connection");
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
            zwp_confined_pointer_v1::Event::Confined => debug!("Recved pointer confined event"),
            zwp_confined_pointer_v1::Event::Unconfined => debug!("Receved pointer unconfined event"),
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
            zwp_locked_pointer_v1::Event::Locked => debug!("Received pointer locked event"),
            zwp_locked_pointer_v1::Event::Unlocked => debug!("Received pointer unlocked event"),
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
                && let Some(ctrl) = state.mouse_ctrl.as_ref()
                && let Err(e) = ctrl.rel_motion(step_x, step_y)
            {
                warn!(error:? = e; "Failed to send native relative motion");
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
