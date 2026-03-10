//! Screen and cursor state model.
use super::gpu_passthrough::GpuPassthrough;
use crate::{
    dbus::listener::Event,
    display::{Error, direct_map::ImportedTexture, software_rasterizer::Swapchain},
    mks_error, mks_trace,
};
use RenderBackend::*;
use relm4::gtk::{
    gdk::{MemoryFormat, MemoryTexture, Texture},
    glib::Bytes,
    prelude::*,
};
use std::{
    hint::{unlikely, unreachable_unchecked},
    num::NonZeroU32,
    os::fd::OwnedFd,
};

const LOG_TARGET: &str = "mks.display.event";

#[derive(Debug, Default, Clone, Copy)]
pub struct DirtyFlags {
    pub frame: bool,
    pub cursor: bool,
}

impl DirtyFlags {
    #[inline]
    pub const fn any(&self) -> bool { self.frame || self.cursor }

    /// Merges other into self.
    #[inline]
    pub fn merge(&mut self, other: DirtyFlags) {
        self.frame |= other.frame;
        self.cursor |= other.cursor;
    }

    /// Sets frame and cursor as dirty.
    #[inline]
    pub fn set_frame_and_cursor_dirty(&mut self) {
        self.frame = true;
        self.cursor = true;
    }

    /// Sets cursor as dirty.
    #[inline]
    pub fn set_cursor_dirty(&mut self) { self.cursor = true; }
}

#[derive(Debug, Clone)]
pub struct CursorState {
    pub texture: Option<MemoryTexture>,
    // Stored as signed because QEMU positions the cursor by image top-left, which may be off-screen.
    pub x: i32,
    pub y: i32,
    pub visible: bool,
    pub hot_x: u32,
    pub hot_y: u32,
    pub last_data: Bytes,
}

impl Default for CursorState {
    fn default() -> Self {
        Self {
            last_data: Bytes::from_static(&[]),
            texture: Option::None,
            x: 0,
            y: 0,
            visible: false,
            hot_x: 0,
            hot_y: 0,
        }
    }
}

impl CursorState {
    /// Fast-path check for cursor image reuse.
    ///
    /// Compares metadata first, then validates a few key sample points to avoid
    /// rebuilding the texture when the cursor payload is effectively unchanged.
    #[inline]
    pub fn looks_same(
        &self, width: NonZeroU32, height: NonZeroU32, hot_x: u32, hot_y: u32, new_data: &[u8], bytes_per_pixel: usize,
    ) -> bool {
        let bpp = bytes_per_pixel;
        if unlikely(bpp == 0) {
            return false;
        }
        let invalid_cond = self.hot_x != hot_x
            || self.hot_y != hot_y
            || self.last_data.len() != new_data.len()
            || self.texture.as_ref().map(|t| t.width()).unwrap_or(-1) != width.get() as i32
            || self.texture.as_ref().map(|t| t.height()).unwrap_or(-1) != height.get() as i32;
        if unlikely(invalid_cond) {
            return false;
        }
        let w = width.get() as usize;
        let h = height.get() as usize;
        let stride = w * bpp;
        if unlikely(w < 3 || h < 3) {
            return self.last_data == new_data;
        }
        let points = [
            0,
            (w - 1) * bpp,
            (h - 1) * stride,
            (h - 1) * stride + (w - 1) * bpp,
            (h / 2) * stride + (w / 2) * bpp,
            (w / 2) * bpp,
            (h - 1) * stride + (w / 2) * bpp,
            (h / 2) * stride,
            (h / 2) * stride + (w - 1) * bpp,
        ];
        for &offset in &points {
            if offset + bpp <= new_data.len() && self.last_data[offset..offset + bpp] != new_data[offset..offset + bpp]
            {
                return false;
            }
        }
        true
    }
}

#[derive(Default, Debug)]
pub enum RenderBackend {
    #[default]
    None,
    SoftwareRasterizer(Swapchain),
    DirectMapped(ImportedTexture),
    GpuPassthrough(GpuPassthrough),
}

impl RenderBackend {
    /// Ensures `SoftwareRasterizer` backend exists.
    ///
    /// Returns `(swapchain, created_now)`:
    /// - `swapchain`: active software swapchain.
    /// - `created_now`: `true` if backend was initialized by this call.
    #[inline]
    pub fn ensure_software_rasterizer(&mut self) -> (&mut Swapchain, bool) {
        let mut created = false;
        if !matches!(self, SoftwareRasterizer(_)) {
            *self = SoftwareRasterizer(Swapchain::new());
            created = true;
        }
        let SoftwareRasterizer(swapchain) = self else { unsafe { unreachable_unchecked() } };
        (swapchain, created)
    }

    /// Ensures `DirectMapped` backend exists.
    ///
    /// Returns `(cache, created_now)`:
    /// - `cache`: active imported texture cache.
    /// - `created_now`: `true` if backend was initialized by this call.
    #[inline]
    pub fn ensure_direct_mapped(&mut self) -> (&mut ImportedTexture, bool) {
        let mut created = false;
        if !matches!(self, DirectMapped(_)) {
            *self = DirectMapped(ImportedTexture::new());
            created = true;
        }
        let DirectMapped(cache) = self else { unsafe { unreachable_unchecked() } };
        (cache, created)
    }
}

#[derive(Debug, Default)]
pub struct Screen {
    pub cursor: CursorState,
    pub backend: RenderBackend,
    pub y0_top: bool,
}

impl Screen {
    pub fn new() -> Self { Self::default() }

    /// Applies one display event and returns dirty flags for frame/cursor refresh.
    #[inline]
    pub fn handle_event(&mut self, event: Event) -> Result<DirtyFlags, Error> {
        use Event::*;
        let mut flags = DirtyFlags::default();
        match event {
            Scanout { width, height, stride, pixman_format, data } => {
                self.y0_top = false;
                mks_trace!(
                    "Scanout: {width}x{height}, stride={stride}, pixman=0x{pixman_format:08x}, bytes={}",
                    data.len()
                );
                let (swapchain, _) = self.backend.ensure_software_rasterizer();
                swapchain.full_update_texture(width, height, stride, pixman_format, &data)?;
            }
            Update { x, y, width, height, stride, pixman_format, data } => {
                mks_trace!(
                    "Update: rect=({x},{y} {width}x{height}), stride={stride}, pixman=0x{pixman_format:08x}, bytes={}",
                    data.len()
                );
                let (swapchain, new_created) = self.backend.ensure_software_rasterizer();
                if new_created {
                    // Ignore initial Update that arrives before Scanout (QEMU event ordering race)
                    mks_trace!("Ignoring Update: Software backend not yet initialized");
                    return Ok(flags);
                }
                swapchain.partial_update_texture(x, y, width, height, stride, pixman_format, &data)?;
                flags.frame = true;
            }
            ScanoutDmabuf { dmabuf, width, height, stride, fourcc, modifier, y0_top } => {
                self.y0_top = y0_top;
                mks_trace!(
                    "ScanoutDMABUF: {width}x{height}, stride={stride}, fourcc=0x{fourcc:08x}, \
                     modifier=0x{modifier:016x}, y0_top={y0_top}"
                );
                let fd: OwnedFd = dmabuf.into();
                if let GpuPassthrough(gpu) = &mut self.backend {
                    gpu.stage_single_plane(fd, width, height, stride, fourcc, modifier);
                } else {
                    let mut gpu = GpuPassthrough::new();
                    gpu.stage_single_plane(fd, width, height, stride, fourcc, modifier);
                    self.backend = GpuPassthrough(gpu);
                }
                mks_trace!("ScanoutDMABUF staged; waiting for UpdateDMABUF commit");
            }
            ScanoutDmabuf2 { dmabuf, width, height, stride, fourcc, modifier, offset, y0_top, .. } => {
                self.y0_top = y0_top;
                mks_trace!(
                    "ScanoutDMABUF2: {width}x{height}, planes={}, fourcc=0x{fourcc:08x}, modifier=0x{modifier:016x}, \
                     y0_top={y0_top}",
                    dmabuf.len()
                );
                let fds: Box<_> = dmabuf.into_iter().map(OwnedFd::from).collect();
                if let GpuPassthrough(gpu) = &mut self.backend {
                    gpu.stage_multi_plane(fds, width, height, &stride, &offset, fourcc, modifier);
                } else {
                    let mut gpu = GpuPassthrough::new();
                    gpu.stage_multi_plane(fds, width, height, &stride, &offset, fourcc, modifier);
                    self.backend = GpuPassthrough(gpu);
                }
                mks_trace!("ScanoutDMABUF2 staged; waiting for UpdateDMABUF commit");
            }
            ScanoutMap { memfd, offset, width, height, stride, pixman_format } => {
                self.y0_top = false;
                mks_trace!(
                    "ScanoutMap: {width}x{height}, stride={stride}, offset={offset}, pixman=0x{pixman_format:08x}"
                );
                let (cache, _) = self.backend.ensure_direct_mapped();
                cache.update_texture(memfd.into(), offset, width, height, stride, pixman_format)?;
            }
            UpdateMap { x, y, width, height } => {
                mks_trace!("UpdateMap: rect=({x},{y} {width}x{height})");
                let (_, new_created) = self.backend.ensure_direct_mapped();
                if new_created {
                    // Ignore initial UpdateMap that arrives before ScanoutMap (QEMU event ordering race)
                    mks_trace!("Ignoring UpdateMap: DirectMapped backend not yet initialized");
                    return Ok(flags);
                }
                flags.frame = true;
            }
            UpdateDmabuf { x, y, width, height } => {
                mks_trace!("UpdateDMABUF: rect=({x},{y} {width}x{height})");
                let GpuPassthrough(gpu) = &mut self.backend else {
                    // Ignore initial UpdateDMABUF that arrives before ScanoutDMABUF* (QEMU event ordering race)
                    mks_trace!("Ignoring UpdateDMABUF: GpuPassthrough not yet initialized");
                    return Ok(flags);
                };
                match gpu.commit_update(x, y, width, height) {
                    Ok(true) => flags.frame = true,
                    Ok(false) => {
                        mks_trace!(
                            "Skipping frame signal after UpdateDMABUF: commit was a no-op (invalid damage or missing \
                             active/pending frame)"
                        );
                    }
                    Err(e) => {
                        mks_error!(
                            error:? = e;
                            "Failed to commit DMABUF update; keeping previous texture"
                        );
                    }
                }
            }
            CursorDefine { width, height, hot_x, hot_y, data } => {
                if !self.cursor.looks_same(width, height, hot_x, hot_y, &data, 4) {
                    let data = Bytes::from_owned(data.0);
                    let texture = MemoryTexture::new(
                        width.get().try_into().unwrap(),
                        height.get().try_into().unwrap(),
                        MemoryFormat::B8g8r8a8,
                        &data,
                        (width.get() * 4) as usize,
                    );
                    self.cursor.texture = Some(texture);
                    self.cursor.last_data = data;
                    self.cursor.hot_x = hot_x;
                    self.cursor.hot_y = hot_y;
                    flags.cursor = true;
                }
                self.cursor.visible = true;
            }
            MouseSet { x, y, on } => {
                if self.cursor.x != x || self.cursor.y != y || self.cursor.visible != on {
                    self.cursor.x = x;
                    self.cursor.y = y;
                    self.cursor.visible = on;
                    flags.cursor = true;
                }
            }
            Disable => {
                self.y0_top = false;
                self.backend = RenderBackend::None;
                self.cursor.visible = false;
                self.cursor.texture = Option::None;
                flags.frame = true;
                flags.cursor = true;
            }
        }
        Ok(flags)
    }

    pub fn get_background_texture(&self) -> Option<&Texture> {
        match &self.backend {
            SoftwareRasterizer(sw) => sw.active_texture().as_ref(),
            GpuPassthrough(gpu) => gpu.texture(),
            DirectMapped(cache) => cache.texture(),
            None => Option::None,
        }
    }

    /// Returns `(width, height)` in pixels for the current backend.
    ///
    /// - `width`: frame width.
    /// - `height`: frame height.
    pub fn resolution(&self) -> (u32, u32) {
        match &self.backend {
            SoftwareRasterizer(sw) => sw.resolution(),
            GpuPassthrough(gpu) => gpu.resolution(),
            DirectMapped(cache) => cache.resolution(),
            None => (0, 0),
        }
    }
}
