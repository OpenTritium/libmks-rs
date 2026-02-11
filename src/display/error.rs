use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("System call failed: {0}")]
    System(#[from] rustix::io::Errno),
    #[error("I/O operation failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("Failed to create GDK texture: {0}")]
    Texture(#[from] relm4::gtk::glib::Error),
    #[error("Unsupported Pixman format: {0}")]
    InvalidFormat(#[from] super::pixman_4cc::UnknownPixmanFormat),
    #[error("Invalid state: {0}")]
    State(&'static str),
}
