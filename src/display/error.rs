use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("System call failed: {0}")]
    System(#[from] rustix::io::Errno),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Failed to create GDK texture: {0}")]
    Texture(#[from] relm4::gtk::glib::Error),

    /// Returned when the guest sends a DRM FourCC format that has no GDK MemoryFormat mapping.
    #[error("Unsupported DRM FourCC format (no GDK MemoryFormat mapping): {0}")]
    InvalidFormat(#[from] super::pixman_4cc::UnknownFourccFormat),

    /// Returned when converting from Pixman format to DRM FourCC fails.
    #[error("Unsupported Pixman format (no DRM FourCC mapping): {0}")]
    UnknownPixman(#[from] super::pixman_4cc::UnknownPixmanFormat),

    /// mmap() returned null or the mapping was otherwise invalid.
    #[error("Invalid memory mapping (mmap failed or returned null)")]
    InvalidMapping,

    /// Attempted to redraw() without any buffer staged via import().
    #[error("No buffer staged (call import() first)")]
    NoStagedBuffer,
    #[error("Partial update pixman format does not match staged surface")]
    PartialUpdatePixmanNotMatch,

    #[error("Partial update coordinates are outside surface bounds")]
    PartialUpdateOffScreen,
}
