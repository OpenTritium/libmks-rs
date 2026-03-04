use super::{
    Error,
    pixman_4cc::{FourCC, sanitize_opaque_fourcc},
    udma::{DmabufPlane, build_dmabuf_texture_planar},
};
use crate::mks_debug;
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
    // Skip the first UpdateDMABUF rebuild right after a fresh ScanoutDMABUF import.
    fresh_from_scanout: bool,
}

impl GpuPassthrough {
    fn build_texture(
        width: u32, height: u32, raw_fourcc: FourCC, modifier: u64, planes: &[DmabufPlane],
    ) -> Result<(Texture, FourCC), Error> {
        let fourcc = match sanitize_opaque_fourcc(raw_fourcc) {
            Ok(sanitized_fourcc) => sanitized_fourcc,
            Err(raw_fourcc) => {
                mks_debug!("FourCC not in opaque-sanitization allowlist; keeping original format");
                raw_fourcc
            }
        };
        build_dmabuf_texture_planar(width, height, fourcc, modifier, planes)
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
        let (texture, fourcc) = Self::build_texture(width, height, raw_fourcc, modifier, &gdk_planes)?;
        Ok(Self { texture, planes, fourcc, modifier, width, height, fresh_from_scanout: true })
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
        let (texture, fourcc) = Self::build_texture(width, height, raw_fourcc, modifier, &gdk_planes)?;
        Ok(Self { texture, planes, fourcc, modifier, width, height, fresh_from_scanout: true })
    }

    #[inline]
    pub fn rebuild_texture(&mut self) -> Result<(), Error> {
        if self.fresh_from_scanout {
            self.fresh_from_scanout = false;
            return Ok(());
        }
        let gdk_planes: Box<[_]> = self
            .planes
            .iter()
            .map(|plane| DmabufPlane { fd: plane.fd.as_raw_fd(), stride: plane.stride, offset: plane.offset })
            .collect();
        self.texture = build_dmabuf_texture_planar(self.width, self.height, self.fourcc, self.modifier, &gdk_planes)?;
        self.fresh_from_scanout = false;
        Ok(())
    }

    #[inline]
    pub const fn texture(&self) -> &Texture { &self.texture }

    #[inline]
    pub const fn resolution(&self) -> (u32, u32) { (self.width, self.height) }
}
