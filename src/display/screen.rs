//! Screen and cursor state model.
use super::{gpu_passthrough::GpuPassthrough, pixman_4cc::Pixman};
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
use std::{hint::unreachable_unchecked, os::fd::OwnedFd};

const LOG_TARGET: &str = "mks.display.event";

#[derive(Debug, Default, Clone, Copy)]
pub struct DirtyFlags {
    pub frame: bool,
    pub cursor: bool,
}

impl DirtyFlags {
    #[inline]
    pub const fn any(&self) -> bool { self.frame || self.cursor }
}

#[derive(Debug, Clone)]
pub struct CursorState {
    pub texture: Option<MemoryTexture>,
    pub x: i32,
    pub y: i32,
    pub visible: bool,
    pub hot_x: i32,
    pub hot_y: i32,
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
        &self, width: i32, height: i32, hot_x: i32, hot_y: i32, new_data: &[u8], bytes_per_pixel: usize,
    ) -> bool {
        if self.hot_x != hot_x
            || self.hot_y != hot_y
            || self.texture.as_ref().map(|t| t.width()).unwrap_or(-1) != width
            || self.texture.as_ref().map(|t| t.height()).unwrap_or(-1) != height
            || self.last_data.len() != new_data.len()
        {
            return false;
        }
        let w = width as usize;
        let h = height as usize;
        let stride = w * bytes_per_pixel;
        if w < 3 || h < 3 {
            return self.last_data == new_data;
        }
        let points = [
            0,
            (w - 1) * 4,
            (h - 1) * stride,
            (h - 1) * stride + (w - 1) * 4,
            (h / 2) * stride + (w / 2) * 4,
            (w / 2) * 4,
            (h - 1) * stride + (w / 2) * 4,
            (h / 2) * stride,
            (h / 2) * stride + (w - 1) * 4,
        ];
        for &offset in &points {
            if offset + 4 <= new_data.len() && self.last_data[offset..offset + 4] != new_data[offset..offset + 4] {
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
    pub fn new() -> Self { Self { cursor: CursorState::default(), backend: None, y0_top: false } }

    /// Applies one display event and returns dirty flags for frame/cursor refresh.
    #[inline]
    pub fn handle_event(&mut self, event: Event) -> Result<DirtyFlags, Error> {
        use Event::*;
        let mut flags = DirtyFlags::default();
        match event {
            Scanout { width, height, stride, pixman_format, data } => {
                let bytes = data.len();
                mks_trace!("Scanout: {width}x{height}, stride={stride}, pixman=0x{pixman_format:08x}, bytes={bytes}");
                self.y0_top = false;
                let pixman = Pixman::from(pixman_format);
                let (swapchain, _) = self.backend.ensure_software_rasterizer();
                swapchain.full_update_texture(width, height, stride, pixman, &data)?;
                flags.frame = true;
            }
            Update { x, y, width, height, stride, pixman_format, data } => {
                let bytes = data.len();
                mks_trace!(
                    "Update: rect=({x},{y} {width}x{height}), stride={stride}, pixman=0x{pixman_format:08x}, \
                     bytes={bytes}"
                );
                if x < 0 || y < 0 || width <= 0 || height <= 0 {
                    mks_error!("Ignoring invalid QEMU Update rect: x={x}, y={y}, width={width}, height={height}");
                    return Ok(flags);
                }
                let pixman = Pixman::from(pixman_format);
                let (swapchain, new_created) = self.backend.ensure_software_rasterizer();
                if new_created {
                    return Err(Error::State(
                        "Received partial 'Update' without preceding 'Scanout' (Software Backend uninitialized)",
                    ));
                }
                let x = x as u32;
                let y = y as u32;
                let width = width as u32;
                let height = height as u32;
                swapchain.partial_update_texture(x, y, width, height, stride, pixman, &data)?;
                flags.frame = true;
            }
            ScanoutDmabuf { dmabuf, width, height, stride, fourcc, modifier, y0_top } => {
                mks_trace!(
                    "ScanoutDMABUF: {width}x{height}, stride={stride}, fourcc=0x{fourcc:08x}, \
                     modifier=0x{modifier:016x}, y0_top={y0_top}"
                );
                let fd: OwnedFd = dmabuf.into();
                match GpuPassthrough::from_single_plane(fd, width, height, stride, fourcc, modifier) {
                    Ok(gpu) => {
                        self.y0_top = y0_top;
                        self.backend = GpuPassthrough(gpu);
                    }
                    Err(e) => {
                        mks_error!(
                            error:? = e;
                            "Failed to import ScanoutDmabuf (fourcc=0x{fourcc:08x}, \
                             modifier=0x{modifier:016x}); keeping previous frame"
                        );
                    }
                }
            }
            ScanoutDmabuf2 { dmabuf, width, height, stride, fourcc, modifier, offset, y0_top, .. } => {
                let planes = dmabuf.len();
                mks_trace!(
                    "ScanoutDMABUF2: {width}x{height}, planes={planes}, fourcc=0x{fourcc:08x}, \
                     modifier=0x{modifier:016x}, y0_top={y0_top}"
                );
                let fds: Vec<OwnedFd> = dmabuf.into_iter().map(OwnedFd::from).collect();
                let offsets_u32: Box<[u32]> =
                    offset.iter().map(|&v| u32::try_from(v).expect("offset exceeds u32::MAX")).collect();
                match GpuPassthrough::from_multi_plane(fds, width, height, stride, &offsets_u32, fourcc, modifier) {
                    Ok(gpu) => {
                        self.y0_top = y0_top;
                        self.backend = GpuPassthrough(gpu);
                    }
                    Err(e) => {
                        mks_error!(
                            error:? = e;
                            "Failed to import ScanoutDmabuf2 (fourcc=0x{fourcc:08x}, \
                             modifier=0x{modifier:016x}); keeping previous frame"
                        );
                    }
                }
            }
            ScanoutMap { memfd, offset, width, height, stride, pixman_format } => {
                mks_trace!(
                    "ScanoutMap: {width}x{height}, stride={stride}, offset={offset}, pixman=0x{pixman_format:08x}"
                );
                self.y0_top = false;
                let (cache, _) = self.backend.ensure_direct_mapped();
                let _texture = cache.update_texture(memfd.into(), offset, width, height, stride, pixman_format)?;
                flags.frame = true;
            }
            UpdateMap { x, y, width, height } => {
                mks_trace!("UpdateMap: rect=({x},{y} {width}x{height})");
                if x < 0 || y < 0 || width <= 0 || height <= 0 {
                    mks_error!("Ignoring invalid QEMU UpdateMap rect: x={x}, y={y}, width={width}, height={height}");
                    return Ok(flags);
                }
                let (_, new_created) = self.backend.ensure_direct_mapped();
                if new_created {
                    return Err(Error::State(
                        "Received partial 'UpdateMap' without preceding 'ScanoutMap' (DirectMapped Backend \
                         uninitialized)",
                    ));
                }
                flags.frame = true;
            }
            UpdateDmabuf { x, y, width, height } => {
                mks_trace!("UpdateDMABUF: rect=({x},{y} {width}x{height})");
                if width <= 0 || height <= 0 {
                    mks_error!("Ignoring invalid QEMU UpdateDmabuf rect: x={x}, y={y}, width={width}, height={height}");
                    return Ok(flags);
                }
                // DMABUF content may be updated in-place. gdk::Texture is immutable,
                // so recreate a lightweight wrapper to force GTK/GSK cache invalidation.
                // Forward damage rect so GDK can reuse unchanged regions.
                let GpuPassthrough(gpu) = &mut self.backend else {
                    return Err(Error::State(
                        "Received partial 'UpdateDmabuf' without preceding 'ScanoutDmabuf'/'ScanoutDmabuf2' \
                         (GpuPassthrough Backend uninitialized)",
                    ));
                };
                match gpu.rebuild_texture(x, y, width, height) {
                    Ok(true) => {
                        flags.frame = true;
                    }
                    Ok(false) => {
                        mks_trace!("Skipping frame update signal after UpdateDMABUF: texture unchanged");
                    }
                    Err(e) => {
                        mks_error!(
                            error:? = e;
                            "Failed to rebuild DMABUF texture after UpdateDmabuf event; keeping previous texture"
                        );
                    }
                }
            }
            CursorDefine { width, height, hot_x, hot_y, data } => {
                if !self.cursor.looks_same(width, height, hot_x, hot_y, &data, 4) {
                    let data = Bytes::from_owned(data.0);
                    let texture =
                        MemoryTexture::new(width, height, MemoryFormat::B8g8r8a8, &data, (width as u32 * 4) as usize);
                    self.cursor.texture = Some(texture);
                    self.cursor.last_data = data;
                    self.cursor.hot_x = hot_x;
                    self.cursor.hot_y = hot_y;
                    flags.cursor = true;
                }
                self.cursor.visible = true;
            }
            MouseSet { x, y, on } => {
                let visible = on != 0;
                if self.cursor.x != x || self.cursor.y != y || self.cursor.visible != visible {
                    self.cursor.x = x;
                    self.cursor.y = y;
                    self.cursor.visible = visible;
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
            GpuPassthrough(gpu) => Some(gpu.texture()),
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
            SoftwareRasterizer(sw) => sw.resolution().unwrap_or((0, 0)),
            GpuPassthrough(gpu) => gpu.resolution(),
            DirectMapped(cache) => cache.resolution().unwrap_or((0, 0)),
            None => (0, 0),
        }
    }
}
