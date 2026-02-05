//! 屏幕和鼠标的 model
use super::pixman_4cc::Pixman;
use crate::{dbus::listener::QemuEvent, display::framebuffer::Swapchain};
use RenderBackend::*;
use relm4::gtk::{
    gdk::{DmabufTextureBuilder, MemoryFormat, MemoryTexture, Texture},
    glib::Bytes,
};
use std::{
    hint::unreachable_unchecked,
    os::fd::{IntoRawFd, OwnedFd},
};

#[derive(Debug, Default, Clone, Copy)]
pub struct UpdateFlags {
    pub frame: bool,
    pub cursor: bool,
}

impl UpdateFlags {
    pub fn any(&self) -> bool { self.frame || self.cursor }
}

#[derive(Debug, Default, Clone)]
pub struct CursorState {
    pub texture: Option<MemoryTexture>,
    pub x: i32,
    pub y: i32,
    pub visible: bool,
    pub hot_x: i32,
    pub hot_y: i32,
}

#[derive(Default, Debug)]
pub enum RenderBackend {
    #[default]
    None,
    Software(Swapchain),
    Hardware {
        texture: Texture,
        width: u32,
        height: u32,
    },
}

impl RenderBackend {
    #[inline]
    pub const fn is_none(&self) -> bool { matches!(self, None) }

    #[inline]
    #[track_caller]
    pub fn ensure_software(&mut self) -> &mut Swapchain {
        if self.is_none() {
            *self = Software(Swapchain::new());
        }
        match self {
            Software(sc) => sc,
            Hardware { .. } => {
                panic!(
                    "Logic Error: Attempted to access Software backend but current state is Hardware. Did you forget \
                     to handle Disable/Reset event?"
                );
            }
            None => unsafe { unreachable_unchecked() },
        }
    }
}

#[derive(Debug, Default)]
pub struct Screen {
    pub cursor: CursorState,
    pub backend: RenderBackend,
}

impl Screen {
    pub fn new() -> Self { Self { cursor: CursorState::default(), backend: None } }

    pub fn handle_event(&mut self, event: QemuEvent) -> anyhow::Result<UpdateFlags> {
        use QemuEvent::*;
        let mut flags = UpdateFlags::default();
        match event {
            Scanout { width, height, stride, pixman_format, data } => {
                let pixman = Pixman::from(pixman_format);
                let swapchain = self.backend.ensure_software();
                swapchain.full_update_texture(width, height, stride, pixman, &data)?;
                flags.frame = true;
            }
            Update { x, y, width, height, stride, pixman_format, data } => {
                let pixman = Pixman::from(pixman_format);
                let swapchain = self.backend.ensure_software();
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
                let builder = DmabufTextureBuilder::new()
                    .set_width(width)
                    .set_height(height)
                    .set_n_planes(1)
                    .set_stride(0, stride)
                    .set_fourcc(fourcc)
                    .set_modifier(modifier);
                let fd: OwnedFd = dmabuf.into();
                let builder = unsafe { builder.set_fd(0, fd.into_raw_fd()) };
                let texture = unsafe { builder.build()? };
                self.backend = Hardware { texture, width, height };
                flags.frame = true;
            }
            ScanoutDmabuf2 { dmabuf, width, height, stride, fourcc, modifier, num_planes, .. } => {
                let builder = DmabufTextureBuilder::new()
                    .set_width(width)
                    .set_height(height)
                    .set_n_planes(num_planes)
                    .set_fourcc(fourcc)
                    .set_modifier(modifier);
                let builder =
                    dmabuf.into_iter().zip(stride.iter()).enumerate().fold(builder, |b, (i, (fd, &plane_stride))| {
                        let fd: OwnedFd = fd.into();
                        let i = i as u32;
                        let b = b.set_stride(i, plane_stride);
                        unsafe { b.set_fd(i, fd.into_raw_fd()) }
                    });
                let texture = unsafe { builder.build()? };
                self.backend = Hardware { texture, width, height };
                flags.frame = true;
            }
            UpdateDmabuf { .. } => {
                flags.frame = true;
            }
            CursorDefine { width, height, hot_x, hot_y, data } => {
                let bytes = Bytes::from(&data);
                let texture = MemoryTexture::new(width, height, MemoryFormat::B8g8r8a8, &bytes, (width * 4) as usize);
                self.cursor.texture = Some(texture);
                self.cursor.hot_x = hot_x;
                self.cursor.hot_y = hot_y;
                self.cursor.visible = true;
                flags.cursor = true;
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
                self.backend = None;
                self.cursor.visible = false;
                flags.frame = true;
                flags.cursor = true;
            }
        }
        Ok(flags)
    }

    pub fn get_background_texture(&self) -> Option<&Texture> {
        match &self.backend {
            Software(sw) => sw.active_texture().as_ref(),
            Hardware { texture, .. } => Some(texture),
            None => Option::None,
        }
    }

    /// 宽 x 高
    pub fn resolution(&self) -> (u32, u32) {
        match &self.backend {
            Software(sw) => sw.resolution().unwrap_or((0, 0)),
            &Hardware { width, height, .. } => (width, height),
            None => (0, 0),
        }
    }
}
