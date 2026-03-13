//! `DBus` proxy for QEMU Mouse interface.
//! <https://www.qemu.org/docs/master/interop/dbus-display.html#org.qemu.Display1.Mouse-section>
use serde_repr::Serialize_repr;
use zbus::{Result, proxy};
use zvariant::Type;

#[proxy(interface = "org.qemu.Display1.Mouse", default_service = "org.qemu", gen_blocking = true)]
pub trait Mouse {
    /// Sends a button press event.
    async fn press(&self, button: Button) -> Result<()>;

    /// Sends a button release event.
    async fn release(&self, button: Button) -> Result<()>;

    /// Sets the absolute mouse pointer position.
    async fn set_abs_position(&self, x: u32, y: u32) -> Result<()>;

    /// Sends a relative mouse motion event.
    async fn rel_motion(&self, dx: i32, dy: i32) -> Result<()>;

    /// Indicates whether the guest input device expects absolute coordinates.
    #[zbus(property)]
    fn is_absolute(&self) -> Result<bool>;
}

/// QEMU D-Bus mouse button identifiers.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, Serialize_repr, Type)]
#[repr(u32)]
pub enum Button {
    Left = 0,
    Middle = 1,
    Right = 2,
    WheelUp = 3,
    WheelDown = 4,
    Side = 5,
    Extra = 6,
}

impl Button {
    /// Converts Xorg button codes to [`Button`].
    pub fn from_xorg(button: u32) -> Option<Self> {
        use Button::*;
        match button {
            1 => Some(Left),
            2 => Some(Middle),
            3 => Some(Right),
            4 => Some(WheelUp),
            5 => Some(WheelDown),
            8 => Some(Side),
            9 => Some(Extra),
            _ => None,
        }
    }
}
