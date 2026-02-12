//! 屏幕和鼠标的 model
use super::pixman_4cc::{FourCC, Pixman};
use crate::{
    dbus::listener::Event,
    display::{
        direct_map::ImportedTexture,
        software_rasterizer::Swapchain,
        udma::{build_dmabuf_texture_planar, DmabufPlane},
        Error,
    },
};
use relm4::gtk::{
    gdk::{MemoryFormat, MemoryTexture, Texture},
    glib::Bytes,
    prelude::*,
};
use std::os::fd::{AsRawFd, OwnedFd};
use RenderBackend::*;

#[derive(Debug, Default, Clone, Copy)]
pub struct UpdateFlags {
    pub frame: bool,
    pub cursor: bool,
}

impl UpdateFlags {
    pub fn any(&self) -> bool { self.frame || self.cursor }
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
    #[inline]
    pub fn looks_same(&self, width: u32, height: u32, hot_x: i32, hot_y: i32, new_data: &[u8]) -> bool {
        if self.hot_x != hot_x
            || self.hot_y != hot_y
            || self.texture.as_ref().map(|t| t.width()).unwrap_or(-1) != width as i32
            || self.texture.as_ref().map(|t| t.height()).unwrap_or(-1) != height as i32
            || self.last_data.len() != new_data.len()
        {
            return false;
        }
        let w = width as usize;
        let h = height as usize;
        let stride = w * 4;
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
    GpuPassthrough {
        texture: Texture,
        width: u32,
        height: u32,
    },
}

impl RenderBackend {
    #[inline]
    pub fn ensure_software_rasterizer(&mut self) -> (&mut Swapchain, bool) {
        let mut created = false;
        if !matches!(self, SoftwareRasterizer(_)) {
            *self = SoftwareRasterizer(Swapchain::new());
            created = true;
        }
        let SoftwareRasterizer(swapchain) = self else { unsafe { std::hint::unreachable_unchecked() } };
        (swapchain, created)
    }

    #[inline]
    pub fn ensure_direct_mapped(&mut self) -> (&mut ImportedTexture, bool) {
        let mut created = false;
        if !matches!(self, DirectMapped(_)) {
            *self = DirectMapped(ImportedTexture::new());
            created = true;
        }
        let DirectMapped(cache) = self else { unsafe { std::hint::unreachable_unchecked() } };
        (cache, created)
    }
}

#[derive(Debug, Default)]
pub struct Screen {
    pub cursor: CursorState,
    pub backend: RenderBackend,
}

impl Screen {
    pub fn new() -> Self { Self { cursor: CursorState::default(), backend: None } }

    pub fn handle_event(&mut self, event: Event) -> Result<UpdateFlags, Error> {
        use Event::*;
        let mut flags = UpdateFlags::default();
        match event {
            Scanout { width, height, stride, pixman_format, data } => {
                let pixman = Pixman::from(pixman_format);
                let (swapchain, _) = self.backend.ensure_software_rasterizer();
                swapchain.full_update_texture(width, height, stride, pixman, &data)?;
                flags.frame = true;
            }
            Update { x, y, width, height, stride, pixman_format, data } => {
                let pixman = Pixman::from(pixman_format);
                let (swapchain, new_created) = self.backend.ensure_software_rasterizer();
                if new_created {
                    return Err(Error::State(
                        "Received partial 'Update' without preceding 'Scanout' (Software Backend uninitialized)",
                    ));
                }
                swapchain.partial_update_texture(
                    x as u32,
                    y as u32,
                    width as u32,
                    height as u32,
                    stride,
                    pixman,
                    &data,
                )?;
                flags.frame = true;
            }
            ScanoutDmabuf { dmabuf, width, height, stride, fourcc, modifier, y0_top: _ } => {
                let fd: OwnedFd = dmabuf.into();
                let plane = DmabufPlane { fd: fd.as_raw_fd(), stride, offset: 0 };
                let texture = build_dmabuf_texture_planar(width, height, FourCC::from(fourcc), modifier, &[plane])?;
                self.backend = GpuPassthrough { texture, width, height };
                flags.frame = true;
            }
            ScanoutDmabuf2 { dmabuf, width, height, stride, fourcc, modifier, offset, .. } => {
                let fds = dmabuf.into_iter().map(OwnedFd::from);
                let planes: Box<_> = fds
                    .zip(stride.iter())
                    .zip(offset.iter())
                    .map(|((fd, &stride), &offset)| DmabufPlane {
                        fd: fd.as_raw_fd(),
                        stride,
                        offset: u32::try_from(offset).expect("offset exceeds u32::MAX"),
                    })
                    .collect();
                let texture = build_dmabuf_texture_planar(width, height, FourCC::from(fourcc), modifier, &planes)?;
                self.backend = GpuPassthrough { texture, width, height };
                flags.frame = true;
            }
            ScanoutMap { memfd, offset, width, height, stride, pixman_format } => {
                let (cache, _) = self.backend.ensure_direct_mapped();
                let _texture = cache.update_texture(memfd.into(), offset, width, height, stride, pixman_format)?;
                flags.frame = true;
            }
            UpdateMap { x, y, width, height } => {
                let (cache, new_created) = self.backend.ensure_direct_mapped();
                if new_created {
                    return Err(Error::State(
                        "Received partial 'UpdateMap' without preceding 'ScanoutMap' (DirectMapped Backend \
                         uninitialized)",
                    ));
                }
                cache.record_damage(x as u32, y as u32, width as u32, height as u32);
                flags.frame = true;
            }
            UpdateDmabuf { .. } => {
                flags.frame = true;
            }
            CursorDefine { width, height, hot_x, hot_y, data } => {
                if !self.cursor.looks_same(width as u32, height as u32, hot_x, hot_y, &data) {
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
            GpuPassthrough { texture, .. } => Some(texture),
            DirectMapped(cache) => cache.texture(),
            None => Option::None,
        }
    }

    /// 宽 x 高
    pub fn resolution(&self) -> (u32, u32) {
        match &self.backend {
            SoftwareRasterizer(sw) => sw.resolution().unwrap_or((0, 0)),
            GpuPassthrough { width, height, .. } => (*width, *height),
            DirectMapped(cache) => cache.resolution().unwrap_or((0, 0)),
            None => (0, 0),
        }
    }
}
