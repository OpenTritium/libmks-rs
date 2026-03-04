//! `DBus` proxy for QEMU MultiTouch interface.
//! <https://www.qemu.org/docs/master/interop/dbus-display.html#org.qemu.Display1.MultiTouch-section>
use serde_repr::Serialize_repr;
use zbus::{Result, proxy};
use zvariant::Type;

#[proxy(interface = "org.qemu.Display1.MultiTouch", default_service = "org.qemu", gen_blocking = true)]
pub trait MultiTouch {
    /// Send a touch gesture event.
    async fn send_event(&self, kind: Kind, num_slot: u64, x: f64, y: f64) -> Result<()>;

    /// The maximum number of slots.
    #[zbus(property)]
    fn max_slots(&self) -> Result<i32>;
}

/// Touch slot transition type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize_repr, Type)]
#[repr(u32)]
pub enum Kind {
    Begin = 0,
    Update = 1,
    End = 2,
    Cancel = 3,
}
