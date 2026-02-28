use super::{
    Error,
    pixman_4cc::{FourCC, sanitize_opaque_fourcc},
    udma::{DmabufPlane, build_dmabuf_texture_planar},
};
use crate::mks_warn;
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
}

impl GpuPassthrough {
    fn build_texture_with_fallback(
        width: u32, height: u32, raw_fourcc: FourCC, modifier: u64, planes: &[DmabufPlane],
    ) -> Result<(Texture, FourCC), Error> {
        let sanitized_fourcc = sanitize_opaque_fourcc(raw_fourcc);
        if sanitized_fourcc != raw_fourcc {
            match build_dmabuf_texture_planar(width, height, sanitized_fourcc, modifier, planes) {
                Ok(texture) => return Ok((texture, sanitized_fourcc)),
                Err(e) => {
                    let raw = u32::from(raw_fourcc);
                    let sanitized = u32::from(sanitized_fourcc);
                    mks_warn!(
                        error:? = e;
                        "Sanitized DMABUF import failed (raw=0x{raw:08x}, sanitized=0x{sanitized:08x}, \
                         modifier=0x{modifier:016x}); retrying with raw fourcc"
                    );
                }
            }
        }
        let texture = build_dmabuf_texture_planar(width, height, raw_fourcc, modifier, planes)?;
        Ok((texture, raw_fourcc))
    }

    #[inline]
    pub fn from_single_plane(
        dmabuf_fd: OwnedFd, width: u32, height: u32, stride: u32, fourcc: u32, modifier: u64,
    ) -> Result<Self, Error> {
        let planes = vec![PlaneDesc { fd: dmabuf_fd, stride, offset: 0 }].into_boxed_slice();
        let raw_fourcc = FourCC::from(fourcc);
        let gdk_planes =
            [DmabufPlane { fd: planes[0].fd.as_raw_fd(), stride: planes[0].stride, offset: planes[0].offset }];
        let (texture, fourcc) = Self::build_texture_with_fallback(width, height, raw_fourcc, modifier, &gdk_planes)?;
        Ok(Self { texture, planes, fourcc, modifier, width, height })
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
        let (texture, fourcc) = Self::build_texture_with_fallback(width, height, raw_fourcc, modifier, &gdk_planes)?;
        Ok(Self { texture, planes, fourcc, modifier, width, height })
    }

    #[inline]
    pub fn rebuild_texture(&mut self) -> Result<(), Error> {
        let gdk_planes: Box<[_]> = self
            .planes
            .iter()
            .map(|plane| DmabufPlane { fd: plane.fd.as_raw_fd(), stride: plane.stride, offset: plane.offset })
            .collect();
        self.texture = build_dmabuf_texture_planar(self.width, self.height, self.fourcc, self.modifier, &gdk_planes)?;
        Ok(())
    }

    #[inline]
    pub const fn texture(&self) -> &Texture { &self.texture }

    #[inline]
    pub const fn resolution(&self) -> (u32, u32) { (self.width, self.height) }
}
