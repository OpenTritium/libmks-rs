//! Direct-mapped scanout buffer management.
//!
//! This module handles memory-mapped framebuffer buffers from QEMU via the
//! `DBusDisplay` interface. Unlike GPU passthrough (DMABUF), these buffers are
//! accessed through a shared memory mapping.
//!
//! ## Lifecycle
//! 1. `ImportedTexture::import()` - stage a new buffer (mmap created)
//! 2. `ImportedTexture::redraw()` - lazily build the GPU texture on first use
//! 3. `ImportedTexture::texture()` - access the current texture for presentation
//!
//! ## Key invariant
//! The texture is NOT created immediately on import; it's built lazily on first
//! redraw(). This allows batching multiple ScanoutMap calls before committing.
use super::{
    Error,
    pixman_4cc::{FourCC, Pixman},
};
use crate::mks_trace;
use relm4::gtk::{
    gdk::{MemoryFormat, MemoryTexture, Texture},
    glib::Bytes,
    prelude::*,
};
use rustix::{
    fs::fstat,
    mm::{MapFlags, ProtFlags, mmap, munmap},
};
use std::{
    fmt,
    num::NonZeroU32,
    os::fd::OwnedFd,
    ptr::{NonNull, null_mut},
    slice::from_raw_parts,
    sync::Arc,
};

#[allow(dead_code)]
const LOG_TARGET: &str = "mks.display.memmap";

/// Shared memory buffer wrapping a QEMU framebuffer mmap.
// Manually implement Debug to avoid deriving `ptr: NonNull<u8>` which has privacy concerns.
pub struct SharedMemory {
    _memfd: OwnedFd,
    ptr: NonNull<u8>,
    cap: usize, // Virtual memory size (total mmap size)
    width: NonZeroU32,
    height: NonZeroU32,
    stride: NonZeroU32,
    offset: u32,
    pixman: Pixman,
}

impl fmt::Debug for SharedMemory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SharedMemory")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("stride", &self.stride)
            .field("offset", &self.offset)
            .field("pixman", &self.pixman)
            .finish()
    }
}

// SAFETY: mmap'd memory in read-only mode is safe to share across threads.
impl SharedMemory {
    /// Creates a new shared memory mapping from a QEMU framebuffer memfd.
    ///
    /// The memfd is typically obtained via `org.qemu.Monitor1.DisplayToplevel::ScanoutMap`.
    /// See: <https://www.qemu.org/docs/master/interop/dbus-display.html>
    pub fn new(
        memfd: OwnedFd, offset: u32, width: NonZeroU32, height: NonZeroU32, stride: NonZeroU32, pixman: Pixman,
    ) -> Result<Self, Error> {
        mks_trace!("ScanoutMap: {width}x{height}, stride={stride}, offset={offset}, pixman={pixman:?}");
        let stat = fstat(&memfd)?;
        let size = stat.st_size as usize;
        let ptr = unsafe { mmap(null_mut(), size, ProtFlags::READ, MapFlags::SHARED, &memfd, 0)? };
        let ptr = NonNull::new(ptr as *mut u8).ok_or(Error::InvalidMapping)?;
        Ok(Self { _memfd: memfd, ptr, cap: size, width, height, stride, offset, pixman })
    }

    /// Framebuffer size in bytes (height * stride).
    #[inline]
    #[allow(clippy::len_without_is_empty)]
    pub const fn len(&self) -> usize { (self.height.get() * self.stride.get()) as usize }

    /// Converts the shared memory into a GPU texture for presentation.
    ///
    /// This performs the pixman -> DRM FourCC -> GDK MemoryFormat conversion chain.
    /// The actual pixel data is not copied; the texture references the mmap'd memory.
    pub fn as_texture(self: &Arc<Self>) -> Result<MemoryTexture, Error> {
        struct RecycleWrapper(Arc<SharedMemory>);
        impl AsRef<[u8]> for RecycleWrapper {
            fn as_ref(&self) -> &[u8] {
                unsafe { from_raw_parts(self.0.ptr.as_ptr().add(self.0.offset as usize), self.0.len()) }
            }
        }

        let fourcc: FourCC = self.pixman.try_into()?;
        let format: MemoryFormat = fourcc.try_into()?;
        mks_trace!("BuildTexture: pixman={:?} -> format={format:?}", self.pixman);
        let this = RecycleWrapper(self.clone());
        let bytes = Bytes::from_owned(this);
        let texture = MemoryTexture::new(
            self.width.get().try_into().unwrap(),
            self.height.get().try_into().unwrap(),
            format,
            &bytes,
            self.stride.get() as usize,
        );
        Ok(texture)
    }

    /// Returns framebuffer resolution as `(width, height)`.
    #[inline]
    pub fn resolution(&self) -> (u32, u32) { (self.width.get(), self.height.get()) }
}

// SAFETY: mmap'd memory in read-only mode is safe to share across threads.
unsafe impl Send for SharedMemory {}
unsafe impl Sync for SharedMemory {}

impl Drop for SharedMemory {
    #[inline]
    fn drop(&mut self) { let _ = unsafe { munmap(self.ptr.as_ptr().cast(), self.cap) }; }
}

/// Staged texture with lazy texture building.
///
/// Supports the same lifecycle as `GpuPassthrough`: stage -> commit (redraw) -> present.
#[derive(Debug, Default)]
pub struct ImportedTexture {
    buf: Option<Arc<SharedMemory>>,
    texture: Option<MemoryTexture>,
}

impl ImportedTexture {
    #[inline]
    pub fn new() -> Self { Self::default() }

    /// Stages a new framebuffer for later presentation.
    ///
    /// Unlike `GpuPassthrough`, we store the raw `SharedMemory` directly rather than
    /// FD + metadata. The texture is NOT built here—it will be built lazily on `redraw()`.
    pub fn import(
        &mut self, memfd: OwnedFd, offset: u32, width: NonZeroU32, height: NonZeroU32, stride: NonZeroU32,
        pixman: Pixman,
    ) -> Result<(), Error> {
        let buf = Arc::new(SharedMemory::new(memfd, offset, width, height, stride, pixman)?);
        self.buf = Some(buf);
        self.texture = None; // Lazy build on redraw
        mks_trace!("Buffer staged: {width}x{height}");
        Ok(())
    }

    /// Builds the texture from the current buffer (commit operation).
    ///
    /// This is the "commit" step in the Prepare->Commit flow. Call this after
    /// receiving the final `UpdateMap` for a frame.
    pub fn redraw(&mut self) -> Result<(), Error> {
        let Some(buf) = &self.buf else { return Err(Error::NoStagedBuffer) };
        let (w, h) = buf.resolution();
        self.texture = Some(buf.as_texture()?);
        mks_trace!("Texture committed: {w}x{h}");
        Ok(())
    }

    /// Returns the current presentation texture, if available.
    #[inline]
    pub fn texture(&self) -> Option<&Texture> { self.texture.as_ref().map(|t| t.upcast_ref::<Texture>()) }

    /// Returns current resolution as `(width, height)`.
    #[inline]
    pub fn resolution(&self) -> (u32, u32) { self.buf.as_ref().map(|b| b.resolution()).unwrap_or_default() }

    /// Clears both buffer and texture.
    #[inline]
    pub fn clear(&mut self) {
        self.buf = None;
        self.texture = None;
    }
}
