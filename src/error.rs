use std::borrow::Cow;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum MksError {
    #[error("D-Bus error: {0}")]
    Dbus(#[from] zbus::Error),

    #[error("D-Bus method call failed: {0}")]
    DbusMethod(Cow<'static, str>),

    #[error("D-Bus connection error: {0}")]
    DbusConnection(Cow<'static, str>),

    #[error("Serialization error: {0}")]
    Zvariant(#[from] zvariant::Error),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Display error: {0}")]
    Display(Cow<'static, str>),

    #[error("Input error: {0}")]
    Input(Cow<'static, str>),

    #[error("Keyboard error: {0}")]
    KeyboardError(Cow<'static, str>),

    #[error("Mouse error: {0}")]
    MouseError(Cow<'static, str>),

    #[error("Screen error: {0}")]
    ScreenError(Cow<'static, str>),

    #[error("Not connected")]
    NotConnected,

    #[error("Not attached")]
    NotAttached,

    #[error("Invalid configuration: {0}")]
    InvalidConfig(Cow<'static, str>),

    #[error("Protocol error: {0}")]
    Protocol(Cow<'static, str>),

    #[error("Device not found")]
    DeviceNotFound,

    #[error("No screen available")]
    NoScreenAvailable,

    #[error("Channel communication error: {0}")]
    KanalSend(#[from] kanal::SendError),
}
