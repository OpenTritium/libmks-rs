use super::{
    Error,
    pixman_4cc::{FourCC, sanitize_opaque_fourcc},
    udma::{Damage, DmabufPlane, build_dmabuf_texture_planar},
};
use crate::{mks_debug, mks_trace};
use relm4::gtk::gdk::Texture;
use std::os::fd::{AsRawFd, OwnedFd};

const LOG_TARGET: &str = "mks.display.gpu_passthrough";

#[derive(Debug)]
struct PlaneDesc {
    fd: OwnedFd,
    stride: u32,
    offset: u32,
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
        width: u32, height: u32, raw_fourcc: FourCC, modifier: u64, planes: &[DmabufPlane],
        update_texture: Option<&Texture>, damage: Option<Damage>,
    ) -> Result<(Texture, FourCC), Error> {
        let fourcc = match sanitize_opaque_fourcc(raw_fourcc) {
            Ok(sanitized_fourcc) => sanitized_fourcc,
            Err(raw_fourcc) => {
                mks_debug!("FourCC not in opaque-sanitization allowlist; keeping original format");
                raw_fourcc
            }
        };
        build_dmabuf_texture_planar(width, height, fourcc, modifier, planes, update_texture, damage)
            .map(|t| (t, fourcc))
            .map_err(Error::Texture)
    }

    #[inline]
    pub fn from_single_plane(
        dmabuf_fd: OwnedFd, width: u32, height: u32, stride: u32, fourcc: u32, modifier: u64,
    ) -> Result<Self, Error> {
        let planes = vec![PlaneDesc { fd: dmabuf_fd, stride, offset: 0 }].into_boxed_slice();
        let raw_fourcc = FourCC::from(fourcc);
        let gdk_planes =
            [DmabufPlane { fd: planes[0].fd.as_raw_fd(), stride: planes[0].stride, offset: planes[0].offset }];
        let (texture, fourcc) = Self::build_texture(width, height, raw_fourcc, modifier, &gdk_planes, None, None)?;
        Ok(Self { texture, planes, fourcc, modifier, width, height, pending_presentation: true })
    }

    #[inline]
    pub fn from_multi_plane(
        dmabuf_fds: Vec<OwnedFd>, width: u32, height: u32, plane_strides: Vec<u32>, plane_offsets: &[u32], fourcc: u32,
        modifier: u64,
    ) -> Result<Self, Error> {
        let planes: Box<[PlaneDesc]> = dmabuf_fds
            .into_iter()
            .zip(plane_strides)
            .zip(plane_offsets.iter().copied())
            .map(|((fd, stride), offset)| PlaneDesc { fd, stride, offset })
            .collect();
        let raw_fourcc = FourCC::from(fourcc);
        let gdk_planes: Box<[_]> = planes
            .iter()
            .map(|plane| DmabufPlane { fd: plane.fd.as_raw_fd(), stride: plane.stride, offset: plane.offset })
            .collect();
        let (texture, fourcc) = Self::build_texture(width, height, raw_fourcc, modifier, &gdk_planes, None, None)?;
        Ok(Self { texture, planes, fourcc, modifier, width, height, pending_presentation: true })
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
        let gdk_planes: Box<[_]> = self
            .planes
            .iter()
            .map(|plane| DmabufPlane { fd: plane.fd.as_raw_fd(), stride: plane.stride, offset: plane.offset })
            .collect();
        self.texture = build_dmabuf_texture_planar(
            self.width,
            self.height,
            self.fourcc,
            self.modifier,
            &gdk_planes,
            Some(&self.texture),
            Some(damage),
        )?;
        mks_trace!("DMABUF texture rebuild finished");
        self.pending_presentation = false;
        Ok(true)
    }

    #[inline]
    pub const fn texture(&self) -> &Texture { &self.texture }

    #[inline]
    pub const fn resolution(&self) -> (u32, u32) { (self.width, self.height) }
}
