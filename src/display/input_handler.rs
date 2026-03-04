use super::{
    coordinate::Coordinate,
    input_daemon::{InputCommand, WatchCommand},
};
use crate::{
    dbus::{keyboard::PressAction, mouse::Button, multitouch::Kind},
    keymaps::Qnum,
    mks_debug, mks_error,
};
use InputCommand::*;
use kanal::{AsyncSender, Sender};
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
        if let Err(e) = tx.try_send(WatchCommand::Update(cap)) {
            mks_error!(error:? = e; "Failed to update capability watchers");
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
        if let Err(e) = tx.send(MouseSetAbs(x, y)) {
            mks_error!(error:? = e; "Failed to queue absolute mouse move");
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
        if let Err(e) = tx.send(MouseRel(dx, dy)) {
            mks_error!(error:? = e; "Failed to queue relative mouse move");
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
            let Err(e) = tx.send(MousePress(btn)) else {
                let Err(e) = tx.send(MouseRelease(btn)) else { continue };
                mks_error!(error:? = e; "Failed to release mouse button");
                continue;
            };
            mks_error!(error:? = e; "Failed to press mouse button");
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
        match tx.send(command) {
            Ok(()) => match transition {
                Press => {
                    self.held_keys.insert(qnum);
                }
                Release => {
                    self.held_keys.remove(&qnum);
                }
            },
            Err(e) => {
                mks_error!(error:? = e; "Failed to send keyboard {transition} event");
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
            if let Err(e) = tx.send(KbdRelease(qnum)) {
                mks_error!(error:? = e; "Failed to release tracked key during capture reset");
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
            PressAction::Press => tx.send(MousePress(btn)),
            PressAction::Release => tx.send(MouseRelease(btn)),
        };
        if let Err(e) = result {
            mks_error!(error:? = e; "Failed to send mouse {transition} event");
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
        if let Err(e) = tx.send(Touch { kind, num_slot, x, y }) {
            mks_error!(error:? = e; "Failed to queue touch event");
        }
    }
}
