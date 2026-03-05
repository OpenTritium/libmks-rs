use super::{
    Error,
    pixman_4cc::{FourCC, sanitize_opaque_fourcc},
    udma::{Damage, DmabufPlane, build_dmabuf_texture_planar},
};
use crate::{mks_debug, mks_trace};
use relm4::gtk::{gdk::Texture, prelude::*};
use std::{
    os::fd::{AsRawFd, OwnedFd},
    sync::Arc,
};

const LOG_TARGET: &str = "mks.display.gpu_passthrough";

#[derive(Debug, Clone)]
struct PlaneDesc {
    fd: Arc<OwnedFd>,
    stride: u32,
    offset: u32,
}

/// Pending DMABUF scanout metadata without imported texture.
///
/// This lets us defer GTK/EGL import until `UpdateDmabuf`, so the imported
/// texture reflects a fully rendered guest frame instead of an in-progress one.
#[derive(Debug)]
pub struct DmabufScanout {
    width: u32,
    height: u32,
    planes: Box<[PlaneDesc]>,
    fourcc: FourCC,
    modifier: u64,
}

impl DmabufScanout {
    #[inline]
    pub fn new(
        dmabuf_fds: Vec<OwnedFd>, width: u32, height: u32, plane_strides: Vec<u32>, plane_offsets: &[u32], fourcc: u32,
        modifier: u64,
    ) -> Self {
        let planes: Box<[PlaneDesc]> = dmabuf_fds
            .into_iter()
            .zip(plane_strides)
            .zip(plane_offsets.iter().copied())
            .map(|((fd, stride), offset)| PlaneDesc { fd: Arc::new(fd), stride, offset })
            .collect();
        Self { width, height, planes, fourcc: FourCC::from(fourcc), modifier }
    }

    #[inline]
    pub fn import(self) -> Result<GpuPassthrough, Error> {
        let (texture, fourcc) = GpuPassthrough::build_texture(
            self.width,
            self.height,
            self.fourcc,
            self.modifier,
            &self.planes,
            None,
            None,
        )?;
        Ok(GpuPassthrough {
            texture,
            planes: self.planes,
            fourcc,
            modifier: self.modifier,
            width: self.width,
            height: self.height,
            pending_presentation: true,
        })
    }
}

/// GPU-backed scanout state for DMA-BUF paths.
///
/// This keeps both the imported texture and the plane metadata so `UpdateDMABUF`
/// can rebuild the texture without requiring a fresh `ScanoutDMABUF`.
#[derive(Debug)]
pub struct GpuPassthrough {
    texture: Texture,
    // Keep FD ownership + per-plane layout in one place for rebuilds.
    planes: Box<[PlaneDesc]>,
    fourcc: FourCC,
    modifier: u64,
    width: u32,
    height: u32,
    pending_presentation: bool,
}

impl GpuPassthrough {
    fn build_texture(
        width: u32, height: u32, raw_fourcc: FourCC, modifier: u64, planes: &[PlaneDesc],
        update_texture: Option<&Texture>, damage: Option<Damage>,
    ) -> Result<(Texture, FourCC), Error> {
        let fourcc = match sanitize_opaque_fourcc(raw_fourcc) {
            Ok(sanitized_fourcc) => sanitized_fourcc,
            Err(raw_fourcc) => {
                mks_debug!("FourCC not in opaque-sanitization allowlist; keeping original format");
                raw_fourcc
            }
        };

        let gdk_planes: Box<[_]> = planes
            .iter()
            .map(|plane| DmabufPlane { fd: plane.fd.as_raw_fd(), stride: plane.stride, offset: plane.offset })
            .collect();

        let texture = build_dmabuf_texture_planar(width, height, fourcc, modifier, &gdk_planes, update_texture, damage)
            .map_err(Error::Texture)?;

        // Keep DMABUF fds alive at least as long as the GDK texture.
        let fds: Vec<Arc<OwnedFd>> = planes.iter().map(|plane| plane.fd.clone()).collect();
        // SAFETY: The key is process-unique for this attachment and the value is `'static`,
        // so storing it on the texture object is valid for the whole object lifetime.
        unsafe {
            texture.set_data("mks-dmabuf-fds", fds);
        }

        Ok((texture, fourcc))
    }

    #[inline]
    fn clip_damage(&self, x: i32, y: i32, width: i32, height: i32) -> Option<Damage> {
        if width <= 0 || height <= 0 {
            return None;
        }
        let max_x = i64::from(self.width);
        let max_y = i64::from(self.height);
        let x = i64::from(x);
        let y = i64::from(y);
        let x0 = x.clamp(0, max_x);
        let y0 = y.clamp(0, max_y);
        let x1 = (x + i64::from(width)).clamp(0, max_x);
        let y1 = (y + i64::from(height)).clamp(0, max_y);
        if x1 <= x0 || y1 <= y0 {
            return None;
        }
        Some(Damage { x: x0 as u32, y: y0 as u32, width: (x1 - x0) as u32, height: (y1 - y0) as u32 })
    }

    #[inline]
    pub fn rebuild_texture(&mut self, x: i32, y: i32, width: i32, height: i32) -> Result<bool, Error> {
        if self.pending_presentation {
            mks_trace!("DMABUF texture rebuild skipped: first update after fresh scanout import");
            self.pending_presentation = false;
            // The first UpdateDMABUF after ScanoutDMABUF is the first safe present point.
            // Texture can be reused, but caller still must render this frame.
            return Ok(true);
        }
        let Some(damage) = self.clip_damage(x, y, width, height) else {
            mks_trace!("DMABUF texture rebuild skipped: clipped damage is empty, rect=({x},{y} {width}x{height})");
            return Ok(false);
        };
        mks_trace!(
            "DMABUF texture rebuilding: frame={}x{}, damage=({},{} {}x{})",
            self.width,
            self.height,
            damage.x,
            damage.y,
            damage.width,
            damage.height
        );
        let (texture, _) = Self::build_texture(
            self.width,
            self.height,
            self.fourcc,
            self.modifier,
            &self.planes,
            Some(&self.texture),
            Some(damage),
        )?;
        self.texture = texture;
        mks_trace!("DMABUF texture rebuild finished");
        self.pending_presentation = false;
        Ok(true)
    }

    #[inline]
    pub const fn texture(&self) -> &Texture { &self.texture }

    #[inline]
    pub const fn resolution(&self) -> (u32, u32) { (self.width, self.height) }
}
