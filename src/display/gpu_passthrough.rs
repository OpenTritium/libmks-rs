use super::{
    Error,
    pixman_4cc::{FourCC, sanitize_opaque_fourcc},
    udma::{Damage, DmabufPlane, build_dmabuf_texture_planar},
};
use crate::{mks_debug, mks_trace};
use relm4::gtk::gdk::Texture;
use rustix::fs::fstat;
use std::os::fd::{AsRawFd, OwnedFd};

const LOG_TARGET: &str = "mks.display.gpu_passthrough";
type DmabufId = (u64, u64);

#[derive(Debug)]
struct PlaneDesc {
    fd: OwnedFd,
    dmabuf_id: DmabufId,
    stride: u32,
    offset: u32,
}

#[inline]
fn get_dmabuf_id(fd: &OwnedFd) -> Result<DmabufId, Error> {
    let stat = fstat(fd)?;
    Ok((stat.st_dev, stat.st_ino))
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
        let dmabuf_id = get_dmabuf_id(&dmabuf_fd)?;
        let planes = vec![PlaneDesc { fd: dmabuf_fd, dmabuf_id, stride, offset: 0 }].into_boxed_slice();
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
            .map(|((fd, stride), offset)| {
                let dmabuf_id = get_dmabuf_id(&fd)?;
                Ok(PlaneDesc { fd, dmabuf_id, stride, offset })
            })
            .collect::<Result<Vec<_>, Error>>()?
            .into_boxed_slice();
        let raw_fourcc = FourCC::from(fourcc);
        let gdk_planes: Box<[_]> = planes
            .iter()
            .map(|plane| DmabufPlane { fd: plane.fd.as_raw_fd(), stride: plane.stride, offset: plane.offset })
            .collect();
        let (texture, fourcc) = Self::build_texture(width, height, raw_fourcc, modifier, &gdk_planes, None, None)?;
        Ok(Self { texture, planes, fourcc, modifier, width, height, pending_presentation: true })
    }

    #[inline]
    pub(super) fn is_equivalent(
        &self, dmabuf_fds: &[OwnedFd], width: u32, height: u32, plane_strides: &[u32], plane_offsets: &[u32],
        fourcc: u32, modifier: u64,
    ) -> Result<bool, Error> {
        let raw_fourcc = FourCC::from(fourcc);
        let incoming_fourcc = sanitize_opaque_fourcc(raw_fourcc).unwrap_or(raw_fourcc);
        if self.width != width || self.height != height {
            mks_trace!(
                "DMABUF changed: reason=resolution current={}x{} incoming={}x{}",
                self.width,
                self.height,
                width,
                height
            );
            return Ok(false);
        }
        if self.fourcc != incoming_fourcc {
            mks_trace!(
                "DMABUF changed: reason=fourcc current=0x{:08x} incoming=0x{:08x}",
                *self.fourcc,
                *incoming_fourcc
            );
            return Ok(false);
        }
        if self.modifier != modifier {
            mks_trace!("DMABUF changed: reason=modifier current=0x{:016x} incoming=0x{:016x}", self.modifier, modifier);
            return Ok(false);
        }
        if self.planes.len() != dmabuf_fds.len()
            || self.planes.len() != plane_strides.len()
            || self.planes.len() != plane_offsets.len()
        {
            mks_trace!(
                "DMABUF changed: reason=plane_count current={} incoming_fds={} incoming_strides={} incoming_offsets={}",
                self.planes.len(),
                dmabuf_fds.len(),
                plane_strides.len(),
                plane_offsets.len()
            );
            return Ok(false);
        }
        for ((idx, incoming_fd), (&incoming_stride, &incoming_offset)) in
            dmabuf_fds.iter().enumerate().zip(plane_strides.iter().zip(plane_offsets.iter()))
        {
            let plane = &self.planes[idx];
            if plane.stride != incoming_stride || plane.offset != incoming_offset {
                mks_trace!(
                    "DMABUF changed: reason=plane_layout plane={} current_stride={} incoming_stride={} \
                     current_offset={} incoming_offset={}",
                    idx,
                    plane.stride,
                    incoming_stride,
                    plane.offset,
                    incoming_offset
                );
                return Ok(false);
            }
            let (incoming_dev, incoming_ino) = get_dmabuf_id(incoming_fd)?;
            let (current_dev, current_ino) = plane.dmabuf_id;
            if plane.dmabuf_id != (incoming_dev, incoming_ino) {
                mks_trace!(
                    "DMABUF changed: reason=plane_identity plane={} current_dev={} current_ino={} incoming_dev={} \
                     incoming_ino={}",
                    idx,
                    current_dev,
                    current_ino,
                    incoming_dev,
                    incoming_ino
                );
                return Ok(false);
            }
        }
        Ok(true)
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
