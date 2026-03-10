use super::{
    coordinate::Coordinate,
    input_daemon::{InputCommand, WatchCommand},
    vm_display::{CaptureEvent, GrabShortcut, Message, VmDisplayModel},
};
use crate::{
    dbus::{keyboard::PressAction, mouse::Button, multitouch::Kind},
    keymaps::Qnum,
    mks_debug, mks_error, mks_warn,
};
use InputCommand::*;
use gdk4_wayland::glib::Propagation;
use kanal::{AsyncSender, Sender};
use relm4::{
    ComponentSender,
    adw::ToastOverlay,
    gtk::{
        DrawingArea, EventControllerKey, EventControllerMotion, EventControllerScroll, EventControllerScrollFlags,
        GestureClick, prelude::*,
    },
};
use std::collections::HashSet;
use typed_builder::TypedBuilder;

const LOG_TARGET: &str = "mks.display.input";

#[derive(Debug, Clone, Copy)]
pub struct Capability {
    pub keyboard: bool,
    pub mouse: bool,
    pub multitouch: bool,
}

impl Default for Capability {
    fn default() -> Self { Self { keyboard: true, mouse: true, multitouch: true } }
}

#[derive(TypedBuilder)]
pub struct InputHandler {
    #[builder(default = 0.)]
    scroll_accumulator_y: f64,
    #[builder(default, setter(strip_option))]
    pub input_cmd_tx: Option<Sender<InputCommand>>, // Keep sync sender: relm4 `update` is non-async.
    #[builder(default, setter(strip_option))]
    pub watch_cmd_tx: Option<AsyncSender<WatchCommand>>, // Push capability updates and watcher shutdown.
    #[builder(default)]
    pub capability: Capability,
    #[builder(default = false)]
    pub is_absolute: bool,
    #[builder(default = HashSet::with_capacity(8))]
    held_keys: HashSet<Qnum>,
}

impl InputHandler {
    #[inline]
    pub const fn input_cmd_tx(&self) -> Option<&Sender<InputCommand>> { self.input_cmd_tx.as_ref() }

    #[inline]
    pub const fn set_mouse_mode(&mut self, is_absolute: bool) { self.is_absolute = is_absolute; }

    /// Updates device capabilities and notifies the property watcher manager.
    #[inline]
    pub fn update_capabilities(&mut self, cap: Capability) {
        self.capability = cap;
        let Some(tx) = &self.watch_cmd_tx else {
            mks_error!("Watch command channel unavailable; skipping capability update");
            return;
        };
        let result = tx.try_send(WatchCommand::Update(cap));
        match result {
            Err(e) => mks_error!(error:? = e; "Failed to send capability update {cap:?} to watch channel"),
            Ok(false) => mks_warn!("Skipped capability update {cap:?} because watch command channel was full"),
            Ok(true) => (),
        }
    }

    #[inline]
    pub fn set_abs_position(&self, x: u32, y: u32) {
        if !self.capability.mouse {
            mks_error!("Mouse capability disabled; cannot set absolute position");
            return;
        }
        let Some(tx) = &self.input_cmd_tx else {
            mks_error!("Input command channel unavailable; dropping absolute move command");
            return;
        };
        let result = tx.try_send(MouseSetAbs(x, y));
        match result {
            Err(e) => mks_error!(error:? = e; "Failed to queue absolute mouse move to ({x}, {y})"),
            Ok(false) => mks_warn!("Dropped absolute mouse move to ({x}, {y}) because input command channel was full"),
            Ok(true) => (),
        }
    }

    #[inline]
    pub fn rel_motion(&self, dx: i32, dy: i32) {
        if !self.capability.mouse {
            mks_error!("Mouse capability disabled; cannot send relative motion");
            return;
        }
        let Some(tx) = &self.input_cmd_tx else {
            mks_error!("Input command channel unavailable; dropping relative move command");
            return;
        };
        let result = tx.try_send(MouseRel(dx, dy));
        match result {
            Err(e) => mks_error!(error:? = e; "Failed to queue relative mouse move by ({dx}, {dy})"),
            Ok(false) => {
                mks_warn!("Dropped relative mouse move by ({dx}, {dy}) because input command channel was full")
            }
            Ok(true) => (),
        }
    }

    #[inline]
    pub fn move_mouse_to(&mut self, widget_x: f32, widget_y: f32, coord: &Coordinate) {
        if !self.capability.mouse {
            mks_error!("Mouse capability disabled; ignoring pointer move request");
            return;
        }
        mks_debug!(
            "Pointer move request: widget=({:.1}, {:.1}), absolute_mode={}",
            widget_x,
            widget_y,
            self.is_absolute
        );
        if !self.is_absolute {
            mks_debug!("Ignoring widget motion in relative mode; waiting for native relative events");
            return;
        }
        let Some((guest_x, guest_y)) = coord.widget_to_guest(widget_x, widget_y) else {
            mks_error!("Failed to map widget coordinates ({widget_x}, {widget_y}); dropping motion event");
            return;
        };
        self.set_abs_position(guest_x, guest_y);
    }

    #[inline]
    pub const fn cache_mouse_scroll(&mut self, dy: f64) -> i64 {
        let acc = &mut self.scroll_accumulator_y;
        *acc += dy;
        let steps = acc.trunc() as i64;
        *acc -= steps as f64;
        steps
    }

    #[inline]
    pub fn scroll_mouse(&self, steps: i64) {
        if !self.capability.mouse {
            mks_error!("Mouse capability disabled; ignoring scroll event");
            return;
        }
        let Some(tx) = &self.input_cmd_tx else {
            mks_error!("Input command channel unavailable; dropping scroll event");
            return;
        };
        for _ in 0..steps.abs() {
            let btn = if steps.is_positive() {
                Button::WheelDown
            } else {
                Button::WheelUp
            };
            let result = tx.try_send(MousePress(btn));
            match result {
                Err(e) => mks_error!(error:? = e; "Failed to queue scroll mouse press for {btn:?} (steps={steps})"),
                Ok(false) => {
                    mks_warn!(
                        "Dropped scroll mouse press for {btn:?} (steps={steps}) because input command channel was full"
                    )
                }
                Ok(true) => (),
            };
            let result = tx.try_send(MouseRelease(btn));
            match result {
                Err(e) => mks_error!(error:? = e; "Failed to queue scroll mouse release for {btn:?} (steps={steps})"),
                Ok(false) => {
                    mks_warn!(
                        "Dropped scroll mouse release for {btn:?} (steps={steps}) because input command channel was \
                         full"
                    )
                }
                Ok(true) => (),
            };
        }
    }

    /// Translates and forwards keyboard press/release events.
    pub fn press_keyboard(&mut self, keycode: u32, transition: PressAction) {
        use PressAction::*;
        if !self.capability.keyboard {
            mks_error!("Keyboard capability disabled; ignoring keyboard event");
            return;
        }
        let qnum = Qnum::from_xorg_keycode(keycode);
        if qnum.is_unmapped() {
            mks_error!("Ignoring unmapped keyboard keycode {keycode}");
            return;
        }
        let command = match transition {
            Press => KbdPress(qnum),
            Release => KbdRelease(qnum),
        };
        let Some(tx) = &self.input_cmd_tx else {
            mks_error!("Input command channel unavailable; dropping keyboard event");
            return;
        };
        match tx.try_send(command) {
            Ok(true) if transition == Press => {
                self.held_keys.insert(qnum);
            }
            Ok(true) => {
                self.held_keys.remove(&qnum);
            }
            Ok(false) => {
                mks_warn!(
                    "Dropped keyboard {transition} event for keycode {keycode} ({qnum:?}) because input command \
                     channel was full"
                );
            }
            Err(e) => {
                mks_error!(
                    error:? = e;
                    "Failed to send keyboard {transition} event for keycode {keycode} ({qnum:?})"
                );
            }
        }
    }

    /// Releases all tracked keyboard keys to prevent stuck modifiers when capture is dropped.
    pub fn release_all_keys(&mut self) {
        if !self.capability.keyboard {
            self.held_keys.clear();
            mks_debug!("Keyboard capability disabled; clearing tracked keys");
            return;
        }
        let Some(tx) = &self.input_cmd_tx else {
            mks_error!("Input command channel unavailable; cannot release tracked keys");
            return;
        };
        for qnum in self.held_keys.drain() {
            let result = tx.try_send(KbdRelease(qnum));
            match result {
                Err(e) => mks_error!(error:? = e; "Failed to release tracked key {qnum:?} during capture reset"),
                Ok(false) => {
                    mks_warn!(
                        "Dropped tracked key release for {qnum:?} during capture reset because input command channel \
                         was full"
                    )
                }
                Ok(true) => (),
            }
        }
    }

    pub fn press_mouse_button(&self, button: u32, transition: PressAction) {
        if !self.capability.mouse {
            mks_error!("Mouse capability disabled; ignoring mouse button event");
            return;
        }
        let Some(btn) = Button::from_xorg(button) else {
            mks_error!("Ignoring unmapped mouse button {button}");
            return;
        };
        let Some(tx) = &self.input_cmd_tx else {
            mks_error!("Input command channel unavailable; dropping mouse button event");
            return;
        };
        let result = match transition {
            PressAction::Press => tx.try_send(MousePress(btn)),
            PressAction::Release => tx.try_send(MouseRelease(btn)),
        };
        match result {
            Err(e) => mks_error!(error:? = e; "Failed to send mouse {transition} event for button {button} ({btn:?})"),
            Ok(false) => {
                mks_warn!(
                    "Dropped mouse {transition} event for button {button} ({btn:?}) because input command channel was \
                     full"
                )
            }
            Ok(true) => (),
        }
    }

    /// Forwards touch events to the input daemon.
    pub fn touch(&self, kind: Kind, num_slot: u64, x: f64, y: f64) {
        if !self.capability.multitouch {
            mks_error!("Multitouch capability disabled; ignoring touch event");
            return;
        }
        let Some(tx) = &self.input_cmd_tx else {
            mks_error!("Input command channel unavailable; dropping touch event");
            return;
        };
        let result = tx.try_send(Touch { kind, num_slot, x, y });
        match result {
            Err(e) => {
                mks_error!(error:? = e; "Failed to queue touch event kind={kind:?}, slot={num_slot}, pos=({x}, {y})")
            }
            Ok(false) => {
                mks_warn!(
                    "Dropped touch event kind={kind:?}, slot={num_slot}, pos=({x}, {y}) because input command channel \
                     was full"
                )
            }
            Ok(true) => (),
        }
    }
}

/// 挂载所有与输入相关的 GTK Event Controllers 并将事件映射到 Message 转发给主模型
pub fn attach_gtk_controllers(
    input_overlay: &DrawingArea, root: &ToastOverlay, sender: &ComponentSender<VmDisplayModel>,
    grab_shortcut: GrabShortcut,
) {
    let motion_ctrl = EventControllerMotion::new();
    let sender_clone = sender.clone();
    motion_ctrl.connect_motion(move |_, x, y| {
        sender_clone.input(Message::MouseMove { x: x as f32, y: y as f32 });
    });
    let sender_c = sender.clone();
    motion_ctrl.connect_leave(move |_| sender_c.input(Message::MouseLeave));
    input_overlay.add_controller(motion_ctrl);

    let click = GestureClick::new();
    click.set_button(0);
    let sender_clone = sender.clone();
    let input_overlay_click = input_overlay.clone();
    click.connect_pressed(move |gesture, _, x, y| {
        input_overlay_click.grab_focus();
        sender_clone.input(Message::SetConfined(CaptureEvent::Capture { click_pos: Some((x as f32, y as f32)) }));
        sender_clone.input(Message::MouseButton { button: gesture.current_button(), transition: PressAction::Press });
    });
    let sender_clone = sender.clone();
    click.connect_released(move |gesture, _, _, _| {
        sender_clone.input(Message::MouseButton { button: gesture.current_button(), transition: PressAction::Release });
    });
    input_overlay.add_controller(click);

    let scroll = EventControllerScroll::new(EventControllerScrollFlags::VERTICAL);
    let sender_clone = sender.clone();
    scroll.connect_scroll(move |_, _dx, dy| {
        sender_clone.input(Message::Scroll { dy });
        Propagation::Proceed
    });
    input_overlay.add_controller(scroll);

    let key = EventControllerKey::new();
    let sender_for_release = sender.clone();
    let sender_for_key = sender.clone();
    key.connect_key_pressed(move |_, keyval, keycode, modifiers| {
        if modifiers.contains(grab_shortcut.mask) && keyval == grab_shortcut.key {
            sender_for_release.input(Message::SetConfined(CaptureEvent::Release));
            return Propagation::Stop;
        }
        sender_for_key.input(Message::Key { keycode, transition: PressAction::Press });
        Propagation::Stop
    });
    let sender_clone = sender.clone();
    key.connect_key_released(move |_, _keyval, keycode, _| {
        sender_clone.input(Message::Key { keycode, transition: PressAction::Release });
    });
    root.add_controller(key);
}
