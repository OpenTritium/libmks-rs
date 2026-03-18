//! <https://wayland.app/protocols/pointer-constraints-unstable-v1>
use crate::{display::input_event_bus::InputCommand, mks_debug, mks_error, mks_info, mks_warn};
use gdk4_wayland::{
    WaylandDisplay,
    gdk::Display,
    glib::{ControlFlow, IOCondition, SourceId, unix_fd_add_local},
    prelude::*,
    wayland_client::{
        Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum,
        protocol::{
            wl_compositor::WlCompositor,
            wl_pointer::WlPointer,
            wl_region::WlRegion,
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
    os::unix::io::{AsFd, RawFd},
    rc::Rc,
};
use wayland_protocols::wp::{
    pointer_constraints::zv1::client::{
        zwp_confined_pointer_v1::ZwpConfinedPointerV1,
        zwp_locked_pointer_v1::ZwpLockedPointerV1,
        zwp_pointer_constraints_v1::{Lifetime, ZwpPointerConstraintsV1},
    },
    keyboard_shortcuts_inhibit::zv1::client::{
        zwp_keyboard_shortcuts_inhibit_manager_v1::ZwpKeyboardShortcutsInhibitManagerV1,
        zwp_keyboard_shortcuts_inhibitor_v1::ZwpKeyboardShortcutsInhibitorV1,
    },
    relative_pointer::zv1::client::{
        zwp_relative_pointer_manager_v1::ZwpRelativePointerManagerV1,
        zwp_relative_pointer_v1::{self, ZwpRelativePointerV1},
    },
};

const LOG_TARGET: &str = "mks.display.wayland";

pub struct WaylandState {
    pointer_constraints: Option<ZwpPointerConstraintsV1>,
    relative_pointer_manager: Option<ZwpRelativePointerManagerV1>,
    shortcuts_inhibit_manager: Option<ZwpKeyboardShortcutsInhibitManagerV1>,
    compositor: Option<WlCompositor>,
    seat: Option<WlSeat>,
    pointer: Option<WlPointer>,
    shortcuts_inhibitor: Option<ZwpKeyboardShortcutsInhibitorV1>,
    pointer_capture: PointerCapture,
    seat_global: Option<u32>,
    input_tx: Sender<InputCommand>,
    rel_x_residue: f64,
    rel_y_residue: f64,
}

impl WaylandState {
    fn new(input_tx: Sender<InputCommand>) -> Self {
        Self {
            pointer_constraints: None,
            relative_pointer_manager: None,
            shortcuts_inhibit_manager: None,
            compositor: None,
            seat: None,
            pointer: None,
            shortcuts_inhibitor: None,
            pointer_capture: PointerCapture::None,
            seat_global: None,
            input_tx,
            rel_x_residue: 0.,
            rel_y_residue: 0.,
        }
    }
}

#[derive(Default, PartialEq)]
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
    queue: EventQueue<WaylandState>,
    handle: QueueHandle<WaylandState>,
    state: WaylandState,
}

impl WaylandConfine {
    pub fn from_gdk(gdk_display: &WaylandDisplay, input_tx: Sender<InputCommand>) -> Self {
        mks_info!("Initializing Wayland pointer confinement through the GDK bridge");
        let wl_display = gdk_display.wl_display().expect("Failed to get WlDisplay");
        let backend = wl_display.backend().upgrade().expect("Wayland connection is dead");
        let conn = Connection::from_backend(backend);
        let mut queue = conn.new_event_queue();
        let handle = queue.handle();
        let mut state = WaylandState::new(input_tx);
        let display_proxy = conn.display();
        let _registry = display_proxy.get_registry(&handle, ());
        if let Err(e) = queue.roundtrip(&mut state) {
            mks_error!(error:? = e; "Wayland registry roundtrip failed");
        }
        Self { conn, queue, handle, state }
    }

    /// Constrains the pointer to `rect` on the target surface.
    ///
    /// Returns `true` only when pointer capture has been established successfully.
    ///
    /// When `prefer_relative` is true, this requires native relative-pointer protocol.
    /// Otherwise use region confinement for absolute guest mouse mode.
    pub fn confine_pointer(
        &mut self, surface: &WlSurface, (x, y, w, h): (u32, u32, u32, u32), prefer_relative: bool,
    ) -> bool {
        if self.state.pointer_capture != PointerCapture::None {
            mks_warn!("Pointer capture already active; ignoring duplicate confine request");
            return false;
        }
        // Start each capture session from a clean relative-motion residue state.
        self.state.rel_x_residue = 0.;
        self.state.rel_y_residue = 0.;
        let Some(constraints) = self.state.pointer_constraints.as_ref() else {
            mks_error!("Wayland pointer-constraints protocol unavailable; cannot confine pointer");
            return false;
        };
        let Some(pointer) = self.state.pointer.as_ref() else {
            mks_error!("Wayland pointer unavailable; cannot confine pointer");
            return false;
        };
        if prefer_relative {
            let Some(relative_manager) = self.state.relative_pointer_manager.as_ref() else {
                mks_error!("Relative pointer protocol unavailable; cannot enable relative capture");
                return false;
            };
            let relative = relative_manager.get_relative_pointer(pointer, &self.handle, ());
            let locked = constraints.lock_pointer(surface, pointer, None, Lifetime::Persistent, &self.handle, ());
            self.state.pointer_capture = PointerCapture::LockedRelative { locked, relative };
            mks_info!("Pointer locked with native relative motion enabled");
        } else {
            // Absolute guest mode path.
            let Some(compositor) = self.state.compositor.as_ref() else {
                mks_error!("Wayland compositor unavailable; cannot confine pointer in absolute mode");
                return false;
            };
            let region = compositor.create_region(&self.handle, ());
            region.add(x.try_into().unwrap(), y.try_into().unwrap(), w.try_into().unwrap(), h.try_into().unwrap());
            let confined =
                constraints.confine_pointer(surface, pointer, Some(&region), Lifetime::Persistent, &self.handle, ());
            region.destroy();
            self.state.pointer_capture = PointerCapture::Confined(confined);
            mks_info!("Pointer confined to region for absolute guest mouse mode");
        }
        if let Err(e) = self.conn.flush() {
            mks_error!(error:? = e; "Failed to flush Wayland connection");
        }
        true
    }

    pub fn unconfine(&mut self) {
        use PointerCapture::*;
        let mut released = false;
        match mem::take(&mut self.state.pointer_capture) {
            LockedRelative { locked, relative } => {
                relative.destroy();
                locked.destroy();
                mks_info!("Released pointer lock");
                released = true;
            }
            Confined(confined) => {
                confined.destroy();
                mks_info!("Released pointer confinement");
                released = true;
            }
            None => {}
        }
        if !released {
            mks_error!("Pointer capture is not active; nothing to release");
            return;
        }
        if let Err(e) = self.conn.flush() {
            mks_error!(error:? = e; "Failed to flush Wayland connection");
        }
    }

    /// Inhibits compositor/global shortcuts for the given surface.
    ///
    /// Returns `true` only when the inhibitor is active (or already active).
    pub fn inhibit_shortcuts(&mut self, surface: &WlSurface) -> bool {
        if self.state.shortcuts_inhibitor.is_some() {
            return true;
        }
        let Some(manager) = self.state.shortcuts_inhibit_manager.as_ref() else {
            mks_warn!("Wayland keyboard-shortcuts-inhibit protocol unavailable; cannot inhibit shortcuts");
            return false;
        };
        let Some(seat) = self.state.seat.as_ref() else {
            mks_error!("Wayland seat unavailable; cannot inhibit shortcuts");
            return false;
        };
        let inhibitor = manager.inhibit_shortcuts(surface, seat, &self.handle, ());
        self.state.shortcuts_inhibitor = Some(inhibitor);
        if let Err(e) = self.conn.flush() {
            mks_error!(error:? = e; "Failed to flush Wayland connection after shortcut inhibit");
        }
        mks_info!("Keyboard shortcuts inhibited for surface");
        true
    }

    pub fn uninhibit_shortcuts(&mut self) {
        let Some(inhibitor) = self.state.shortcuts_inhibitor.take() else {
            return;
        };
        inhibitor.destroy();
        if let Err(e) = self.conn.flush() {
            mks_error!(error:? = e; "Failed to flush Wayland connection after shortcut uninhibit");
        }
        mks_info!("Keyboard shortcuts inhibition released");
    }

    #[inline]
    pub fn dispatch_pending(&mut self) {
        if let Err(e) = self.queue.dispatch_pending(&mut self.state) {
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
        match event {
            wl_registry::Event::Global { name, interface, version: _ } => {
                match interface.as_str() {
                    "zwp_pointer_constraints_v1" => {
                        state.pointer_constraints = Some(registry.bind(name, 1, qh, ()));
                    }
                    "zwp_keyboard_shortcuts_inhibit_manager_v1" => {
                        state.shortcuts_inhibit_manager = Some(registry.bind(name, 1, qh, ()));
                    }
                    "zwp_relative_pointer_manager_v1" => {
                        state.relative_pointer_manager = Some(registry.bind(name, 1, qh, ()));
                    }
                    "wl_seat" => {
                        // Track a single seat for now; support replacement via GlobalRemove.
                        if state.seat.is_none() {
                            state.seat = Some(registry.bind(name, 1, qh, ()));
                            state.seat_global = Some(name);
                        }
                    }
                    "wl_compositor" => {
                        state.compositor = Some(registry.bind(name, 1, qh, ()));
                    }
                    _ => {}
                }
            }
            wl_registry::Event::GlobalRemove { name } => {
                // Handle dynamic removal of globals so we don't keep stale objects.
                if state.seat_global == Some(name) {
                    mks_info!("Seat global removed; clearing seat, pointer, and shortcuts inhibitor state");
                    state.seat_global = None;
                    state.seat = None;
                    state.pointer = None;
                    state.shortcuts_inhibitor = None;
                    state.pointer_capture = PointerCapture::None;
                }
            }
            _ => {}
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

impl Dispatch<ZwpRelativePointerV1, ()> for WaylandState {
    fn event(
        state: &mut Self, _: &ZwpRelativePointerV1, event: zwp_relative_pointer_v1::Event, _: &(), _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let zwp_relative_pointer_v1::Event::RelativeMotion {
            utime_hi: _,
            utime_lo: _,
            dx,
            dy,
            dx_unaccel: _,
            dy_unaccel: _,
        } = event
        else {
            return;
        };
        state.rel_x_residue += dx;
        state.rel_y_residue += dy;
        let step_x = state.rel_x_residue.trunc() as i32;
        let step_y = state.rel_y_residue.trunc() as i32;
        state.rel_x_residue -= step_x as f64;
        state.rel_y_residue -= step_y as f64;
        if (step_x != 0 || step_y != 0)
            && let Err(e) = state.input_tx.try_send(InputCommand::MouseRel(step_x, step_y))
        {
            mks_error!(error:? = e; "Failed to forward native relative motion; dropping event");
        }
    }
}

macro_rules! empty_dispatch {
    ($($ty:ty),* $(,)?) => {
        $(
            impl Dispatch<$ty, ()> for WaylandState {
                #[inline]
                fn event(
                    _: &mut Self, _: &$ty, _: <$ty as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>,
                ) {
                }
            }
        )*
    };
}

empty_dispatch!(
    ZwpPointerConstraintsV1,
    ZwpKeyboardShortcutsInhibitManagerV1,
    ZwpKeyboardShortcutsInhibitorV1,
    ZwpRelativePointerManagerV1,
    ZwpConfinedPointerV1,
    ZwpLockedPointerV1,
    WlPointer,
    WlCompositor,
    WlRegion,
);

pub struct ConfineState {
    pub wayland_confine: Rc<RefCell<WaylandConfine>>,
    pub poll_source: Option<SourceId>,
}

impl ConfineState {
    pub fn connect_to_wayland(input_tx: kanal::Sender<InputCommand>) -> Option<Self> {
        let display = Display::default()?;
        let wl_display = display.downcast::<WaylandDisplay>().ok()?;
        mks_info!("Wayland session detected; enabling pointer-confinement support");
        let confine = WaylandConfine::from_gdk(&wl_display, input_tx);
        let confine = Rc::new(RefCell::new(confine));
        let fd = confine.borrow().get_conn_raw_fd();
        let confine_clone = confine.clone();
        let poll_source = unix_fd_add_local(fd, IOCondition::IN, move |_fd, _condition| {
            confine_clone.borrow_mut().dispatch_pending();
            ControlFlow::Continue
        });
        mks_debug!("Attached Wayland FD monitor to GLib main context");
        Some(Self { wayland_confine: confine, poll_source: Some(poll_source) })
    }
}

impl Drop for ConfineState {
    fn drop(&mut self) {
        if let Some(source) = self.poll_source.take() {
            source.remove();
        }
        self.wayland_confine.borrow_mut().uninhibit_shortcuts();
        self.wayland_confine.borrow_mut().unconfine();
    }
}
