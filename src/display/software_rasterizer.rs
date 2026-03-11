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
use crate::{mks_debug, mks_error, mks_trace};
use relm4::gtk::gdk::Texture;
use rustix::{
    fs::{MemfdFlags, SealFlags, fcntl_add_seals, ftruncate, memfd_create},
    mm::{MapFlags, ProtFlags, mmap, munmap},
};
use std::{
    io,
    num::{NonZeroU32, NonZeroU64},
    os::fd::{AsRawFd, OwnedFd, RawFd},
    ptr::{self, NonNull},
};

const LOG_TARGET: &str = "mks.display.raster";

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
    pub capacity: NonZeroU64,
    pub width: NonZeroU32,
    pub height: NonZeroU32,
    pub stride: NonZeroU32,
    pub pixman: Pixman,
}

impl UdmaSurface {
    /// Creates and populates a new buffer.
    #[inline]
    pub fn with_buf(
        width: NonZeroU32, height: NonZeroU32, stride: NonZeroU32, pixman: Pixman, buf: &[u8],
    ) -> io::Result<Self> {
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
    pub fn new(width: NonZeroU32, height: NonZeroU32, stride: NonZeroU32, pixman: Pixman) -> io::Result<Self> {
        // Create an anonymous file with sealing support.
        let mem_fd = memfd_create("qemu_surface", MemfdFlags::CLOEXEC | MemfdFlags::ALLOW_SEALING)?;
        let size = (stride.get() * height.get()) as u64;
        // Round up to page size to satisfy mmap requirements.
        let page_size = fetch_page_size() as u64;
        let aligned_size = NonZeroU64::new((size + page_size - 1) & !(page_size - 1))
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "UDMA surface size must be non-zero"))?;
        ftruncate(&mem_fd, aligned_size.get())?;
        // Seal the file to prevent resizing and re-sealing, which ensures safety for the consumer (GPU).
        let seals = SealFlags::SHRINK | SealFlags::GROW | SealFlags::SEAL;
        fcntl_add_seals(&mem_fd, seals)?;
        // Map as SHARED so the `udmabuf` kernel driver sees the data updates.
        let ptr = unsafe {
            mmap(
                ptr::null_mut(),
                aligned_size.get() as usize,
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
            capacity: aligned_size,
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
    pub const fn len(&self) -> usize { (self.height.get() * self.stride.get()) as usize }

    #[inline]
    pub fn dmabuf_fd(&self) -> RawFd { self.dmabuf_fd.as_raw_fd() }

    /// Checks if the buffer capacity is sufficient for the requested dimensions.
    #[inline]
    pub const fn is_reuseable(&self, height: NonZeroU32, stride: NonZeroU32) -> bool {
        self.capacity.get() >= (height.get() * stride.get()) as u64
    }

    /// Checks if the layout matches exactly.
    #[inline]
    pub fn layout_matches(&self, width: NonZeroU32, height: NonZeroU32, stride: NonZeroU32, pixman: Pixman) -> bool {
        self.width == width && self.height == height && self.stride == stride && self.pixman == pixman
    }

    /// Updates only the layout metadata (for internal Swapchain synchronization).
    #[inline]
    pub fn update_layout(&mut self, width: NonZeroU32, height: NonZeroU32, stride: NonZeroU32, pixman: Pixman) {
        debug_assert!(self.is_reuseable(height, stride));
        self.width = width;
        self.height = height;
        self.stride = stride;
        self.pixman = pixman;
    }

    /// Replaces the entire buffer content.
    #[inline]
    pub fn update_all(
        &mut self, width: NonZeroU32, height: NonZeroU32, stride: NonZeroU32, pixman: Pixman, buf: &[u8],
    ) {
        if !self.layout_matches(width, height, stride, pixman) {
            self.update_layout(width, height, stride, pixman);
        }
        debug_assert!(self.len() as u64 <= self.capacity.get());
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
    fn update_rect(
        &mut self, x: u32, y: u32, width: NonZeroU32, height: NonZeroU32, stride: NonZeroU32, buf: *const u8,
    ) {
        let cw = self.width.get();
        let ch = self.height.get();
        let cs = self.stride.get();
        let w = width.get();
        let h = height.get();
        let s = stride.get();
        let is_invalid = x >= cw || { y >= ch || w > cw.saturating_sub(x) || h > ch.saturating_sub(y) };
        if is_invalid {
            mks_error!("Ignoring out-of-bounds partial update: rect=({x},{y} {width}x{height}), surface={cw}x{ch}",);
            return;
        }

        // Sanity check to prevent OOB writes in debug builds
        #[cfg(debug_assertions)]
        {
            let bpp = self.pixman.bytes_per_pixel() as u32;
            let end_offset = (((y + h - 1) * cs) + ((x + w) * bpp)) as usize;
            assert!(end_offset <= self.len(), "Dirty rect exceeds buffer bounds");
        }
        let bpp = self.pixman.bytes_per_pixel() as usize;
        let row_len = w as usize * bpp;
        let dest_base = self.ptr.as_ptr();
        for i in 0..h as usize {
            let dst_offset = ((y as usize + i) * cs as usize) + (x as usize * bpp);
            let src_offset = i * s as usize;
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
        let offset = ((y * active.stride.get()) + (x * bpp as u32)) as usize;
        let src_ptr = unsafe { active.ptr.add(offset) };
        self.update_rect(x, y, width, height, active.stride, src_ptr.as_ptr());
    }
}

impl Drop for UdmaSurface {
    #[inline]
    fn drop(&mut self) {
        // SAFETY: We must use the original aligned allocation size.
        unsafe {
            if let Err(e) = munmap(self.ptr.as_ptr() as *mut _, self.capacity.get() as usize) {
                mks_error!(error:?=e; "Failed to unmap UDMA surface memory during drop")
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

    /// Returns `(width, height)` of the active frame buffer.
    ///
    /// - `width`: active frame width in pixels.
    /// - `height`: active frame height in pixels.
    #[inline]
    pub fn resolution(&self) -> (u32, u32) {
        self.active_buf().as_ref().map(|b| (b.width.get(), b.height.get())).unwrap_or_default()
    }

    /// Propagates changes from the Active buffer to the Shadow buffer.
    #[inline]
    fn sync_active_to_shadow(&mut self) -> Result<bool, Error> {
        let active_idx = self.active_idx();
        let shadow_idx = self.shadow_idx();
        let [dst, src] = unsafe { self.buffers.get_disjoint_unchecked_mut([shadow_idx, active_idx]) };
        let src = match src {
            Some(src) => src,
            None => panic!("Active buffer uninitialized"),
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
        &mut self, w: NonZeroU32, h: NonZeroU32, stride: NonZeroU32, pixman: Pixman, buf: &[u8],
    ) -> Result<Texture, Error> {
        let mk_texture = |fd, fourcc: FourCC| {
            let plane = DmabufPlane { fd, stride, offset: 0 };
            build_dmabuf_texture_planar(w, h, fourcc, DRM_FORMAT_MOD_LINEAR, &[plane], None, None)
        };
        self.last_damage = Some(Damage { x: 0, y: 0, width: w, height: h });
        let needs_alloc = self.shadow_buf_mut().as_ref().is_none_or(|b| !b.is_reuseable(h, stride));
        if needs_alloc {
            let new_buf = UdmaSurface::with_buf(w, h, stride, pixman, buf)?;
            *self.shadow_buf_mut() = Some(new_buf);
        } else {
            self.shadow_buf_mut().as_mut().unwrap().update_all(w, h, stride, pixman, buf);
        }
        let shadow_buf = self.shadow_buf_mut().as_ref().unwrap();
        let fourcc = pixman.try_into()?;
        let texture = mk_texture(shadow_buf.dmabuf_fd(), fourcc)?;
        self.swap_frame();
        *self.active_texture_mut() = Some(texture.clone());
        Ok(texture)
    }

    /// Handles a partial update (dirty rect).
    #[allow(clippy::too_many_arguments)]
    pub fn partial_update_texture(
        &mut self, x: u32, y: u32, w: NonZeroU32, h: NonZeroU32, stride: NonZeroU32, pixman: Pixman, buf: &[u8],
    ) -> Result<Texture, Error> {
        let Some(active) = self.active_buf().as_ref() else {
            panic!("Swapchain uninitialized");
        };
        if pixman != active.pixman {
            panic!("Partial update pixman format mismatch: expected {:x}, got {:x}", active.pixman, pixman);
        }
        let aw = active.width.get();
        let ah = active.height.get();
        let w = w.get();
        let h = h.get();
        if x >= aw || y >= ah {
            // Expected during resize/mode transitions when stale Update events race with new Scanout.
            mks_debug!("Ignoring off-screen partial update: rect=({x},{y} {w}x{h}), surface={aw}x{ah}",);
            // Off-screen update, return previous texture
            return Ok(self.active_texture().as_ref().cloned().expect("Active texture missing"));
        }
        let clipped_width = NonZeroU32::new(w.min(aw - x)).expect("Damage width stays non-zero after clipping");
        let clipped_height = NonZeroU32::new(h.min(ah - y)).expect("Damage height stays non-zero after clipping");
        let cw = clipped_width.get();
        let ch = clipped_height.get();
        if cw != w || ch != h {
            mks_debug!("Clipping partial update: rect=({x},{y} {w}x{h}) -> {cw}x{ch} within surface={aw}x{ah}");
        }
        mks_trace!(
            "Partial texture refresh applying: rect=({x},{y} {clipped_width}x{clipped_height}), src_stride={stride}"
        );
        let s = stride.get() as usize;
        let bpp = pixman.bytes_per_pixel();
        let row_bytes = cw * bpp as u32;
        let required = (ch as usize - 1).checked_mul(s).unwrap() + row_bytes as usize;
        if buf.len() < required {
            mks_debug!(
                "Ignoring partial update: payload too short (need={required}, got={}, rect={cw}x{ch}, stride={s})",
                buf.len()
            );
            // Payload too short, return previous texture
            return Ok(self.active_texture().as_ref().cloned().expect("Active texture missing"));
        }
        // Bring shadow buffer up to date with the previous frame's state
        let texture_invalidated = self.sync_active_to_shadow()?;
        // Queue damage for the *next* frame
        self.last_damage = Some(Damage { x, y, width: clipped_width, height: clipped_height });
        // Apply current frame changes
        let shadow_buf = self.shadow_buf_mut().as_mut().unwrap();
        shadow_buf.update_rect(x, y, clipped_width, clipped_height, stride, buf.as_ptr());
        if texture_invalidated {
            mks_trace!("Partial texture refresh invalidated active texture; rebuilding texture wrapper");
            let fourcc: FourCC = pixman.try_into()?;
            let plane = DmabufPlane { fd: shadow_buf.dmabuf_fd(), stride: shadow_buf.stride, offset: 0 };
            let texture = build_dmabuf_texture_planar(
                shadow_buf.width,
                shadow_buf.height,
                fourcc,
                DRM_FORMAT_MOD_LINEAR,
                &[plane],
                None,
                None,
            )?;
            self.swap_frame();
            *self.active_texture_mut() = Some(texture.clone());
            return Ok(texture);
        }
        mks_trace!("Partial texture refresh finished without texture rebuild");
        // Texture is still valid, flip front/back
        self.swap_frame();
        let texture = self.active_texture().as_ref().cloned().unwrap();
        Ok(texture)
    }
}
