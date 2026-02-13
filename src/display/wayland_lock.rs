//! <https://wayland.app/protocols/relative-pointer-unstable-v1>
use crate::dbus::mouse::MouseController;
use gdk4_wayland::WaylandDisplay;
use log::{info, warn};
use std::{
    cell::RefCell,
    ops::DerefMut,
    os::unix::io::{AsFd, RawFd},
    rc::Rc,
};
use wayland_client::{
    Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum,
    protocol::{
        wl_pointer::{self, WlPointer},
        wl_registry::{self, WlRegistry},
        wl_seat::{self, Capability, WlSeat},
        wl_surface::WlSurface,
    },
};
use wayland_protocols::wp::{
    pointer_constraints::zv1::client::{
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
    relative_pointer_mgr: Option<ZwpRelativePointerManagerV1>,
    seat: Option<WlSeat>,
    pointer: Option<WlPointer>,
    locked_session: Option<LockedPointerSession>,
    mouse_ctrl: Option<MouseController>,
}

pub struct LockedPointerSession {
    pub locked: ZwpLockedPointerV1,
    pub relative: ZwpRelativePointerV1,
}

impl Drop for LockedPointerSession {
    fn drop(&mut self) {
        self.locked.destroy();
        self.relative.destroy();
    }
}

pub struct WaylandLock {
    conn: Connection,
    event_queue: RefCell<EventQueue<WaylandState>>,
    qh: QueueHandle<WaylandState>,
    state: Rc<RefCell<WaylandState>>,
}

impl WaylandLock {
    pub fn from_gdk(gdk_display: &WaylandDisplay, mouse_ctrl: MouseController) -> Self {
        info!("Initializing WaylandLock using GDK safe bridge");
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
        if let Err(e) = event_queue.roundtrip(state.borrow_mut().deref_mut()) {
            warn!(error:? = e; "Roundtrip 1 failed");
        }
        // 等待 Seat 发回 Capabilities -> 触发我们去拿 Pointer
        if let Err(e) = event_queue.roundtrip(state.borrow_mut().deref_mut()) {
            warn!(error:? = e; "Roundtrip 2 failed");
        }
        Self { conn, event_queue: RefCell::new(event_queue), qh, state }
    }

    pub fn lock_pointer(&self, surface: &WlSurface) {
        let state = self.state.borrow();
        let Some(constraints) = state.pointer_constraints.as_ref() else {
            warn!("pointer constraints missing. Locking aborted.");
            return;
        };
        let Some(pointer) = state.pointer.as_ref() else {
            warn!("pointer missing. Locking aborted.");
            return;
        };
        let Some(rel_mgr) = state.relative_pointer_mgr.as_ref() else {
            warn!("relative pointer manager missing. Locking aborted.");
            return;
        };
        if state.locked_session.is_some() {
            warn!("Pointer was locked already");
            return;
        }

        let locked = constraints.lock_pointer(surface, pointer, None, Lifetime::Persistent, &self.qh, ());
        let relative = rel_mgr.get_relative_pointer(pointer, &self.qh, ());
        drop(state);
        info!("Wayland pointer lock engaged");
        self.state.borrow_mut().locked_session = Some(LockedPointerSession { locked, relative });
        if let Err(e) = self.conn.flush() {
            warn!(error:? = e; "Failed to flush connection");
        }
    }

    pub fn unlock(&self, hint: Option<(&WlSurface, f64, f64)>) {
        let mut state = self.state.borrow_mut();

        if let Some(session) = &state.locked_session {
            if let Some((surface, x, y)) = hint {
                session.locked.set_cursor_position_hint(x, y);
                surface.commit();
                info!("🔒 Wayland hint set to ({:.1}, {:.1}) and committed", x, y);
            }

            state.locked_session = None;

            if let Err(e) = self.conn.flush() {
                warn!(error:? = e; "Failed to flush connection during unlock");
            }
            info!("Wayland pointer lock released");
        }
    }

    pub fn dispatch_pending(&self) {
        let mut state = self.state.borrow_mut();
        if let Err(e) = self.event_queue.borrow_mut().dispatch_pending(&mut *state) {
            warn!(error:? = e; "Failed to dispatch pending events");
        }
        if let Err(e) = self.conn.flush() {
            warn!(error:? = e; "Failed to flush connection");
        }
    }

    pub fn get_fd(&self) -> RawFd {
        use std::os::unix::io::AsRawFd;
        self.conn.as_fd().as_raw_fd()
    }
}

impl Dispatch<WlRegistry, ()> for WaylandState {
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
                    state.relative_pointer_mgr = Some(registry.bind(name, 1, qh, ()));
                }
                "wl_seat" => {
                    state.seat = Some(registry.bind(name, 1, qh, ()));
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<WlSeat, ()> for WaylandState {
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

impl Dispatch<ZwpRelativePointerV1, ()> for WaylandState {
    fn event(
        state: &mut Self, _: &ZwpRelativePointerV1, event: zwp_relative_pointer_v1::Event, _: &(), _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwp_relative_pointer_v1::Event::RelativeMotion { dx_unaccel, dy_unaccel, .. } = event
            && let Some(ctrl) = &state.mouse_ctrl
        {
            let ctrl = ctrl.clone();
            relm4::spawn(async move {
                // 使用 unaccelerated delta 避免双重加速
                let _ = ctrl.rel_motion(dx_unaccel as i32, dy_unaccel as i32).await;
            });
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

impl Dispatch<zwp_relative_pointer_manager_v1::ZwpRelativePointerManagerV1, ()> for WaylandState {
    fn event(
        _: &mut Self, _: &ZwpRelativePointerManagerV1, _: zwp_relative_pointer_manager_v1::Event, _: &(),
        _: &Connection, _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<zwp_locked_pointer_v1::ZwpLockedPointerV1, ()> for WaylandState {
    fn event(
        _: &mut Self, _: &ZwpLockedPointerV1, _: zwp_locked_pointer_v1::Event, _: &(), _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_pointer::WlPointer, ()> for WaylandState {
    fn event(_: &mut Self, _: &WlPointer, _: wl_pointer::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}
