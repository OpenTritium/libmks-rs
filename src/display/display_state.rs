//! Screen and cursor state model.
use super::{crop::CropInfo, gpu_passthrough::GpuPassthrough};
use crate::{
    dbus::listener::Event,
    display::{BackendNotReady, Error, memmap::ImportedTexture, software_rasterizer::SoftwareRasterizer},
    mks_error, mks_trace,
};
use RenderBackend::*;
use relm4::gtk::{
    gdk::{MemoryFormat, MemoryTexture, Texture},
    glib::Bytes,
    prelude::*,
};
use std::{
    hint::{likely, unlikely, unreachable_unchecked},
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
    #[inline]
    pub fn looks_same(&self, width: NonZeroU32, height: NonZeroU32, hot_x: u32, hot_y: u32, new_data: &[u8]) -> bool {
        if self.hot_x != hot_x
            || self.hot_y != hot_y
            || self.last_data.len() != new_data.len()
            || self
                .texture
                .as_ref()
                .is_none_or(|t| t.width() != width.get() as i32 || t.height() != height.get() as i32)
        {
            return false;
        }
        self.last_data == new_data
    }
}

#[derive(Default, Debug)]
pub enum RenderBackend {
    #[default]
    None,
    SoftwareRasterizer(SoftwareRasterizer),
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
    pub fn ensure_software_rasterizer(&mut self) -> (&mut SoftwareRasterizer, bool) {
        let mut created = false;
        let not_matched = !matches!(self, SoftwareRasterizer(_));
        if unlikely(not_matched) {
            *self = SoftwareRasterizer(SoftwareRasterizer::new());
            created = true;
        }
        let SoftwareRasterizer(sw) = self else { unsafe { unreachable_unchecked() } };
        (sw, created)
    }

    /// Ensures `DirectMapped` backend exists.
    ///
    /// Returns `(cache, created_now)`:
    /// - `cache`: active imported texture cache.
    /// - `created_now`: `true` if backend was initialized by this call.
    #[inline]
    pub fn ensure_direct_mapped(&mut self) -> (&mut ImportedTexture, bool) {
        let mut created = false;
        let not_matched = !matches!(self, DirectMapped(_));
        if not_matched {
            *self = DirectMapped(ImportedTexture::new());
            created = true;
        }
        let DirectMapped(cache) = self else { unsafe { unreachable_unchecked() } };
        (cache, created)
    }

    /// Returns the current presentation texture, if any.
    #[inline]
    pub fn texture(&self) -> Option<&Texture> {
        match self {
            Self::SoftwareRasterizer(sw) => sw.texture(),
            Self::GpuPassthrough(gpu) => gpu.texture(),
            Self::DirectMapped(map) => map.texture(),
            Self::None => Option::None,
        }
    }

    /// Viewport geometry: (x, y) offset within backing buffer, (width, height) visible area.
    ///
    /// - GPU: returns crop info with potential x/y offset.
    /// - Software/DirectMapped: returns (0, 0, width, height).
    #[inline]
    pub fn crop_info(&self) -> Option<CropInfo> {
        match self {
            Self::GpuPassthrough(gpu) => gpu.crop_info(),
            Self::SoftwareRasterizer(sw) => {
                let (w, h) = sw.resolution().map(|(w, h)| (w.get(), h.get()))?;
                Some(CropInfo::from_width_height(w as f32, h as f32))
            }
            Self::DirectMapped(map) => {
                let (w, h) = map.resolution().map(|(w, h)| (w.get(), h.get()))?;
                Some(CropInfo::from_width_height(w as f32, h as f32))
            }
            Self::None => Option::None,
        }
    }

    /// Returns current resolution as `(width, height)`.
    #[inline]
    pub fn resolution(&self) -> Option<(NonZeroU32, NonZeroU32)> {
        match self {
            Self::SoftwareRasterizer(sw) => sw.resolution(),
            Self::GpuPassthrough(gpu) => gpu.visible_resolution(),
            Self::DirectMapped(cache) => cache.resolution(),
            Self::None => Option::None,
        }
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
                mks_trace!("Scanout: {width}x{height}, stride={stride}, pixman=0x{pixman_format:08x}, bytes={data:?}");
                let (swapchain, _) = self.backend.ensure_software_rasterizer();
                swapchain.full_update_texture(width, height, stride, pixman_format, data)?;
                flags.frame = true;
            }
            Update { x, y, width, height, stride, pixman_format, data } => {
                mks_trace!(
                    "Update: rect=({x},{y} {width}x{height}), stride={stride}, pixman=0x{pixman_format:08x}, \
                     bytes={data:?}"
                );
                let (swapchain, new_created) = self.backend.ensure_software_rasterizer();
                if unlikely(new_created) {
                    return Err(BackendNotReady::Software.into());
                }
                swapchain.partial_update_texture(x, y, width, height, stride, pixman_format, data)?;
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
            ScanoutDmabuf2 {
                dmabuf,
                x,
                y,
                width,
                height,
                stride,
                fourcc,
                modifier,
                offset,
                backing_width,
                backing_height,
                y0_top,
                num_planes,
            } => {
                debug_assert_eq!(num_planes.get() as usize, dmabuf.len());
                debug_assert_eq!(num_planes.get() as usize, stride.len());
                self.y0_top = y0_top;
                mks_trace!(
                    "ScanoutDMABUF2: backing={backing_width}x{backing_height}, crop={width}x{height} at ({x},{y}), \
                     planes={}, fourcc=0x{fourcc:08x}, modifier=0x{modifier:016x}, y0_top={y0_top}",
                    dmabuf.len()
                );
                let fds: Box<_> = dmabuf.into_iter().map(OwnedFd::from).collect();
                if let GpuPassthrough(gpu) = &mut self.backend {
                    gpu.stage_multi_plane(
                        fds,
                        x,
                        y,
                        width,
                        height,
                        backing_width,
                        backing_height,
                        &stride,
                        &offset,
                        fourcc,
                        modifier,
                    );
                } else {
                    let mut gpu = GpuPassthrough::new();
                    gpu.stage_multi_plane(
                        fds,
                        x,
                        y,
                        width,
                        height,
                        backing_width,
                        backing_height,
                        &stride,
                        &offset,
                        fourcc,
                        modifier,
                    );
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
                cache.import(memfd.into(), offset, width, height, stride, pixman_format)?;
            }
            UpdateMap { x, y, width, height } => {
                mks_trace!("UpdateMap: rect=({x},{y} {width}x{height})");
                let (cache, new_created) = self.backend.ensure_direct_mapped();
                if unlikely(new_created) {
                    return Err(BackendNotReady::DirectMapped.into());
                }
                // Redraw the texture from the imported buffer
                cache.redraw()?;
                flags.frame = true;
            }
            UpdateDmabuf { x, y, width, height } => {
                mks_trace!("UpdateDMABUF: rect=({x},{y} {width}x{height})");
                let GpuPassthrough(gpu) = &mut self.backend else {
                    return Err(BackendNotReady::GpuPassthrough.into());
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
                if !self.cursor.looks_same(width, height, hot_x, hot_y, &data) {
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
                let was_moved = self.cursor.x != x || self.cursor.y != y || self.cursor.visible != on;
                if likely(was_moved) {
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

    #[inline]
    pub fn get_background_texture(&self) -> Option<&Texture> { self.backend.texture() }

    /// Viewport geometry: (x, y) offset within backing, (width, height) visible area.
    #[inline]
    pub fn crop_info(&self) -> Option<CropInfo> { self.backend.crop_info() }

    #[inline]
    pub fn resolution(&self) -> Option<(NonZeroU32, NonZeroU32)> { self.backend.resolution() }
}
