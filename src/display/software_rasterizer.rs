//! # UDMABUF Backend
//!
//! Implements the software rasterization path using `memfd` and `udmabuf`.
//!
//! This backend enables **zero-copy** transfer of CPU-rendered guest frames to the
//! host GPU. It utilizes a double-buffered swapchain with dirty rectangle tracking
//! to minimize memory bandwidth and prevent screen tearing.
use super::{
    Error,
    pixman_4cc::{FourCC, Pixman},
    udma::{
        DRM_FORMAT_MOD_LINEAR, Damage, DmabufPlane, build_dmabuf_texture_planar, create_udmabuf_fd,
        utils::fetch_page_size,
    },
};
use log::error;
use relm4::gtk::gdk::Texture;
use rustix::{
    fs::{MemfdFlags, SealFlags, fcntl_add_seals, ftruncate, memfd_create},
    mm::{MapFlags, ProtFlags, mmap, munmap},
};
use std::{
    io,
    os::fd::{AsRawFd, OwnedFd, RawFd},
    ptr::{self, NonNull},
};

/// A shared memory buffer backed by `memfd` and exported as a DMABUF.
///
/// On creation, the underlying `memfd` is sealed (`F_SEAL_SHRINK | F_SEAL_GROW`)
/// to ensure the storage remains stable while exported to the GPU.
#[derive(Debug)]
pub struct UdmaSurface {
    _mem_fd: OwnedFd,
    dmabuf_fd: OwnedFd,
    /// User-space address of the mapped memory.
    pub ptr: NonNull<u8>,
    /// Actual aligned allocation size, tracked for safe `munmap`.
    pub capacity: usize,
    pub width: u32,
    pub height: u32,
    pub stride: usize,
    pub pixman: Pixman,
}

impl UdmaSurface {
    /// Creates and populates a new buffer.
    #[inline]
    pub fn with_buf(width: u32, height: u32, stride: usize, pixman: Pixman, buf: &[u8]) -> io::Result<Self> {
        let this = Self::new(width, height, stride, pixman)?;
        debug_assert_eq!(this.len(), buf.len(), "Buffer size mismatch");
        // SAFETY: `this` is a fresh allocation guaranteed to match `buf` size.
        unsafe {
            buf.as_ptr().copy_to_nonoverlapping(this.ptr.as_ptr(), buf.len());
        }
        Ok(this)
    }

    /// Allocates a new `memfd` wrapped in a `udmabuf`.
    #[inline]
    pub fn new(width: u32, height: u32, stride: usize, pixman: Pixman) -> io::Result<Self> {
        // Create an anonymous file with sealing support.
        let mem_fd = memfd_create("qemu_surface", MemfdFlags::CLOEXEC | MemfdFlags::ALLOW_SEALING)?;
        let size = stride as u64 * height as u64;
        // Round up to page size to satisfy mmap requirements.
        let page_size = fetch_page_size() as u64;
        let aligned_size = (size + page_size - 1) & !(page_size - 1);
        ftruncate(&mem_fd, aligned_size)?;
        // Seal the file to prevent resizing and re-sealing, which ensures safety for the consumer (GPU).
        let seals = SealFlags::SHRINK | SealFlags::GROW | SealFlags::SEAL;
        fcntl_add_seals(&mem_fd, seals)?;
        // Map as SHARED so the `udmabuf` kernel driver sees the data updates.
        let ptr = unsafe {
            mmap(
                ptr::null_mut(),
                aligned_size as usize,
                ProtFlags::READ | ProtFlags::WRITE,
                MapFlags::SHARED,
                &mem_fd,
                0,
            )?
        };
        // Register the memfd with the udmabuf driver to get a distinct DMABUF FD.
        // This FD can be imported by GDK/EGL.
        let dmabuf_fd = create_udmabuf_fd(&mem_fd, 0, aligned_size)?;
        Ok(Self {
            _mem_fd: mem_fd,
            dmabuf_fd,
            ptr: unsafe { NonNull::new_unchecked(ptr as *mut u8) },
            capacity: aligned_size as usize,
            width,
            height,
            stride,
            pixman,
        })
    }

    #[inline]
    pub fn try_clone(&self) -> io::Result<Self> {
        let this = Self::new(self.width, self.height, self.stride, self.pixman)?;
        // SAFETY: Src and Dst have identical layout by construction.
        unsafe {
            this.ptr.copy_from_nonoverlapping(self.ptr, this.len());
        }
        Ok(this)
    }

    /// Logical size of the pixel buffer.
    #[inline]
    #[allow(clippy::len_without_is_empty)]
    pub const fn len(&self) -> usize { self.height as usize * self.stride }

    #[inline]
    pub fn dmabuf_fd(&self) -> RawFd { self.dmabuf_fd.as_raw_fd() }

    /// Checks if the buffer capacity is sufficient for the requested dimensions.
    #[inline]
    pub const fn is_reuseable(&self, height: u32, stride: usize) -> bool { self.capacity >= height as usize * stride }

    /// Checks if the layout matches exactly.
    #[inline]
    pub fn layout_matches(&self, width: u32, height: u32, stride: usize, pixman: Pixman) -> bool {
        self.width == width && self.height == height && self.stride == stride && self.pixman == pixman
    }

    /// Updates only the layout metadata (for internal Swapchain synchronization).
    #[inline]
    pub fn update_layout(&mut self, width: u32, height: u32, stride: usize, pixman: Pixman) {
        debug_assert!(self.is_reuseable(height, stride));
        self.width = width;
        self.height = height;
        self.stride = stride;
        self.pixman = pixman;
    }

    /// Replaces the entire buffer content.
    #[inline]
    pub fn update_all(&mut self, width: u32, height: u32, stride: usize, pixman: Pixman, buf: &[u8]) {
        if !self.layout_matches(width, height, stride, pixman) {
            self.update_layout(width, height, stride, pixman);
        }
        debug_assert!(self.len() <= self.capacity);
        debug_assert_eq!(self.len(), buf.len(), "Buffer length mismatch");
        unsafe {
            ptr::copy_nonoverlapping(buf.as_ptr(), self.ptr.as_ptr(), buf.len());
        }
    }

    /// Updates a specific dirty region.
    ///
    /// # Safety
    /// Caller ensures `(x, y, width, height)` is within bounds and `buf` is valid.
    #[inline]
    fn update_rect(&mut self, x: u32, y: u32, width: u32, height: u32, stride: usize, buf: *const u8) {
        debug_assert!((x + width) <= self.width);
        debug_assert!((y + height) <= self.height);
        let bpp = self.pixman.bytes_per_pixel();
        // Sanity check to prevent OOB writes in debug builds
        #[cfg(debug_assertions)]
        {
            let end_offset = ((y + height - 1) as usize * self.stride) + ((x + width) as usize * bpp);
            assert!(end_offset <= self.len(), "Dirty rect exceeds buffer bounds");
        }
        let row_len = width as usize * bpp;
        let dest_base = self.ptr.as_ptr();
        for i in 0..height as usize {
            let dst_offset = ((y as usize + i) * self.stride) + (x as usize * bpp);
            let src_offset = i * stride;
            unsafe {
                ptr::copy_nonoverlapping(buf.add(src_offset), dest_base.add(dst_offset), row_len);
            }
        }
    }

    /// Patches this buffer (Shadow) using damage from the Active buffer.
    #[inline]
    pub fn update_damage(&mut self, d: Damage, active: &Self) {
        debug_assert!(self.is_reuseable(active.height, active.stride));
        debug_assert!(self.pixman == active.pixman);
        let Damage { x, y, width, height } = d;
        let bpp = self.pixman.bytes_per_pixel();
        // Compute offset into the Active buffer
        let offset = (y as usize * active.stride) + (x as usize * bpp);
        let src_ptr = unsafe { active.ptr.add(offset) };
        self.update_rect(x, y, width, height, active.stride, src_ptr.as_ptr());
    }
}

impl Drop for UdmaSurface {
    #[inline]
    fn drop(&mut self) {
        // SAFETY: We must use the original aligned allocation size.
        unsafe {
            if let Err(e) = munmap(self.ptr.as_ptr() as *mut _, self.capacity) {
                error!(error:?=e; "Failed to unmap memory")
            }
        }
    }
}

/// Manages the `Active` (front) and `Shadow` (back) frame buffers.
#[derive(Debug)]
pub struct Swapchain {
    buffers: [Option<UdmaSurface>; 2],
    texture_cache: [Option<Texture>; 2],
    active_frame: usize,
    last_damage: Option<Damage>,
}

impl Default for Swapchain {
    fn default() -> Self { Self::new() }
}

impl Swapchain {
    #[inline]
    pub const fn new() -> Self {
        Self { buffers: [None, None], active_frame: 0, texture_cache: [None, None], last_damage: None }
    }

    #[inline]
    const fn active_idx(&self) -> usize { self.active_frame % 2 }

    #[inline]
    const fn shadow_idx(&self) -> usize { self.active_idx() ^ 1 }

    #[inline]
    pub fn active_texture(&self) -> &Option<Texture> { unsafe { self.texture_cache.get_unchecked(self.active_idx()) } }

    #[inline]
    pub fn active_texture_mut(&mut self) -> &mut Option<Texture> {
        let active_idx = self.active_idx();
        unsafe { self.texture_cache.get_unchecked_mut(active_idx) }
    }

    #[inline]
    pub fn active_buf(&self) -> &Option<UdmaSurface> { unsafe { self.buffers.get_unchecked(self.active_idx()) } }

    #[inline]
    pub fn shadow_buf(&self) -> &Option<UdmaSurface> { unsafe { self.buffers.get_unchecked(self.shadow_idx()) } }

    #[inline]
    pub fn shadow_buf_mut(&mut self) -> &mut Option<UdmaSurface> {
        let shadow_idx = self.shadow_idx();
        unsafe { self.buffers.get_unchecked_mut(shadow_idx) }
    }

    #[inline]
    pub fn resolution(&self) -> Option<(u32, u32)> { self.active_buf().as_ref().map(|b| (b.width, b.height)) }

    /// Propagates changes from the Active buffer to the Shadow buffer.
    #[inline]
    fn sync_active_to_shadow(&mut self) -> Result<bool, Error> {
        let active_idx = self.active_idx();
        let shadow_idx = self.shadow_idx();
        let [dst, src] = unsafe { self.buffers.get_disjoint_unchecked_mut([shadow_idx, active_idx]) };
        let src = match src {
            Some(src) => src,
            None => return Err(Error::State("Active buffer uninitialized")),
        };
        if let Some(d) = self.last_damage {
            match dst {
                Some(dst) if dst.is_reuseable(src.height, src.stride) => {
                    if dst.layout_matches(src.width, src.height, src.stride, src.pixman) {
                        dst.update_damage(d, src);
                        return Ok(false);
                    }
                    dst.update_layout(src.width, src.height, src.stride, src.pixman);
                    unsafe {
                        dst.ptr.copy_from_nonoverlapping(src.ptr, src.len());
                    }
                    return Ok(true);
                }
                _ => {
                    *dst = Some(src.try_clone()?);
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    #[inline]
    pub const fn swap_frame(&mut self) { self.active_frame = self.active_frame.wrapping_add(1) }

    /// Handles a full-frame update (e.g. initial frame or resize).
    #[inline]
    pub fn full_update_texture(
        &mut self, width: u32, height: u32, stride: u32, pixman: Pixman, buf: &[u8],
    ) -> Result<Texture, Error> {
        let mk_texture = |fd, fourcc: FourCC| {
            let plane = DmabufPlane { fd, stride, offset: 0 };
            build_dmabuf_texture_planar(width, height, fourcc, DRM_FORMAT_MOD_LINEAR, &[plane])
        };
        self.last_damage = Some(Damage { x: 0, y: 0, width, height });
        let stride_usize = stride as usize;
        let needs_alloc = self.shadow_buf_mut().as_ref().is_none_or(|b| !b.is_reuseable(height, stride_usize));
        if needs_alloc {
            let new_buf = UdmaSurface::with_buf(width, height, stride_usize, pixman, buf)?;
            *self.shadow_buf_mut() = Some(new_buf);
        } else {
            self.shadow_buf_mut().as_mut().unwrap().update_all(width, height, stride_usize, pixman, buf);
        }
        let shadow_buf = self.shadow_buf_mut().as_ref().unwrap();
        let texture = mk_texture(shadow_buf.dmabuf_fd(), pixman.try_into()?)?;
        self.swap_frame();
        *self.active_texture_mut() = Some(texture.clone());
        Ok(texture)
    }

    /// Handles a partial update (dirty rect).
    #[allow(clippy::too_many_arguments)]
    pub fn partial_update_texture(
        &mut self, x: u32, y: u32, width: u32, height: u32, stride: u32, pixman: Pixman, buf: &[u8],
    ) -> Result<Texture, Error> {
        debug_assert!(height > 0 && stride > 0 && width > 0);
        if self.active_buf().is_none() {
            return Err(Error::State("Swapchain uninitialized"));
        }
        debug_assert_eq!(pixman, self.active_buf().as_ref().unwrap().pixman);
        // Bring shadow buffer up to date with the previous frame's state
        let texture_invalidated = self.sync_active_to_shadow()?;
        // Queue damage for the *next* frame
        self.last_damage = Some(Damage { x, y, width, height });
        // Apply current frame changes
        let shadow_buf = self.shadow_buf_mut().as_mut().unwrap();
        shadow_buf.update_rect(x, y, width, height, stride as usize, buf.as_ptr());
        if texture_invalidated {
            let fourcc: FourCC = pixman.try_into()?;
            let plane = DmabufPlane { fd: shadow_buf.dmabuf_fd(), stride: shadow_buf.stride as u32, offset: 0 };
            let texture = build_dmabuf_texture_planar(
                shadow_buf.width,
                shadow_buf.height,
                fourcc,
                DRM_FORMAT_MOD_LINEAR,
                &[plane],
            )?;
            self.swap_frame();
            *self.active_texture_mut() = Some(texture.clone());
            return Ok(texture);
        }
        // Texture is still valid, flip front/back
        self.swap_frame();
        let texture = self.active_texture().as_ref().cloned().unwrap();
        Ok(texture)
    }
}
