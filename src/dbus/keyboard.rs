//! `DBus` proxy for QEMU Keyboard interface.
//! <https://www.qemu.org/docs/master/interop/dbus-display.html#org.qemu.Display1.Keyboard>
use crate::keymaps::Qnum;
use bitflags::bitflags;
use std::fmt;
use zbus::{Result, proxy};

#[proxy(interface = "org.qemu.Display1.Keyboard", default_service = "org.qemu", gen_blocking = true)]
pub trait Keyboard {
    /// Sends a key press event.
    async fn press(&self, qnum: Qnum) -> Result<()>;

    /// Sends a key release event.
    async fn release(&self, qnum: Qnum) -> Result<()>;

    /// The current keyboard modifier state (NumLock, CapsLock, ScrollLock).
    #[zbus(property)]
    fn modifiers(&self) -> Result<LockState>;
}

/// Logical keyboard edge to emit on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PressAction {
    Press,
    Release,
}

impl fmt::Display for PressAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Press => f.write_str("press"),
            Self::Release => f.write_str("release"),
        }
    }
}

bitflags! {
    /// Represents the state of keyboard LEDs/modifiers.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
    pub struct LockState: u32 {
        const SCROLL = 1 << 0;
        const NUM    = 1 << 1;
        const CAPS   = 1 << 2;
    }
}

impl TryFrom<zvariant::OwnedValue> for LockState {
    type Error = zvariant::Error;

    #[inline]
    fn try_from(value: zvariant::OwnedValue) -> zvariant::Result<Self> {
        let val: u32 = value.try_into()?;
        Ok(LockState::from_bits_retain(val))
    }
}
