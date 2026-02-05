use thiserror::Error;

#[derive(Error, Debug)]
pub enum MksError {
    #[error("D-Bus error: {0}")]
    Dbus(#[from] zbus::Error),

    #[error("D-Bus method call failed: {0}")]
    DbusMethod(String),

    #[error("D-Bus connection error: {0}")]
    DbusConnection(String),

    #[error("Serialization error: {0}")]
    Zvariant(#[from] zvariant::Error),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Display error: {0}")]
    Display(String),

    #[error("Input error: {0}")]
    Input(String),

    #[error("Keyboard error: {0}")]
    KeyboardError(String),

    #[error("Mouse error: {0}")]
    MouseError(String),

    #[error("Screen error: {0}")]
    ScreenError(String),

    #[error("Not connected")]
    NotConnected,

    #[error("Not attached")]
    NotAttached,

    #[error("Invalid configuration: {0}")]
    InvalidConfig(String),

    #[error("Protocol error: {0}")]
    Protocol(String),

    #[error("Device not found")]
    DeviceNotFound,

    #[error("No screen available")]
    NoScreenAvailable,

    #[error("Channel communication error: {0}")]
    KanalSend(#[from] kanal::SendError),
}

impl MksError {
    pub fn is_recoverable(&self) -> bool {
        matches!(
            self,
            MksError::Io(_)
                | MksError::Display(_)
                | MksError::Input(_)
                | MksError::ScreenError(_)
                | MksError::KeyboardError(_)
                | MksError::MouseError(_)
        )
    }
}
