use std::borrow::Cow;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum MksError {
    #[error("D-Bus error: {0}")]
    Dbus(#[from] zbus::Error),

    #[error("D-Bus method call failed: {0}")]
    DbusMethod(Cow<'static, str>),

    #[error("D-Bus connection failed: {0}")]
    DbusConnection(Cow<'static, str>),

    #[error("Serialization error: {0}")]
    Zvariant(#[from] zvariant::Error),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Display server error: {0}")]
    Display(Cow<'static, str>),

    #[error("Input subsystem error: {0}")]
    Input(Cow<'static, str>),

    #[error("Keyboard error: {0}")]
    KeyboardError(Cow<'static, str>),

    #[error("Mouse/pointer error: {0}")]
    MouseError(Cow<'static, str>),

    #[error("Screen management error: {0}")]
    ScreenError(Cow<'static, str>),

    #[error("Invalid configuration: {0}")]
    InvalidConfig(Cow<'static, str>),

    #[error("Protocol violation: {0}")]
    Protocol(Cow<'static, str>),

    #[error("Required device not found")]
    DeviceNotFound,

    #[error("Device was disabled by the guest")]
    DeviceWasDisabled,

    #[error("No display screen available (QEMU display not initialized)")]
    NoScreenAvailable,

    #[error("Async channel send failed: {0}")]
    KanalSend(#[from] kanal::SendError),
}
