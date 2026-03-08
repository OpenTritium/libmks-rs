//! DMA-BUF GPU passthrough state machine.
//!
//! This module follows a strict `Prepare (Scanout*) -> Commit (UpdateDMABUF)`
//! flow:
//! - `Scanout*` only stages the latest incoming buffers into `pending`.
//! - `UpdateDMABUF` promotes `pending` to `active` and performs the actual texture import/update.
//!
//! Reference:
//! <https://www.qemu.org/docs/master/interop/dbus-display.html#org-qemu-display1-listener-section>

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

#[derive(Debug)]
struct DmabufState {
    planes: Box<[PlaneDesc]>,
    fourcc: FourCC,
    modifier: u64,
    width: u32,
    height: u32,
}

/// GPU-backed scanout state for DMA-BUF paths.
///
/// Prepare (`Scanout*`) only stages state. Commit (`UpdateDMABUF`) performs
/// the actual import/rebuild, so scanout bursts collapse into a single build.
#[derive(Debug, Default)]
pub struct GpuPassthrough {
    texture: Option<Texture>,
    active: Option<DmabufState>,
    pending: Option<DmabufState>,
}

impl GpuPassthrough {
    #[inline]
    pub const fn new() -> Self { Self { texture: None, active: None, pending: None } }

    #[inline]
    fn stage_pending(&mut self, planes: Box<[PlaneDesc]>, width: u32, height: u32, fourcc: u32, modifier: u64) {
        self.pending = Some(DmabufState { planes, fourcc: FourCC::from(fourcc), modifier, width, height });
    }

    /// Stages a single-plane scanout frame into `pending`.
    ///
    /// Any previous not-yet-committed `pending` frame is replaced.
    #[inline]
    pub fn stage_single_plane(
        &mut self, dmabuf_fd: OwnedFd, width: u32, height: u32, stride: u32, fourcc: u32, modifier: u64,
    ) {
        let planes: Box<_> = [PlaneDesc { fd: dmabuf_fd, stride, offset: 0 }].into();
        self.stage_pending(planes, width, height, fourcc, modifier);
        mks_trace!("DMABUF prepared (single-plane): {width}x{height}; import deferred until UpdateDMABUF");
    }

    /// Stages a multi-plane scanout frame into `pending`.
    ///
    /// FDs/strides/offsets are paired by plane index; any previous `pending`
    /// frame is replaced.
    #[allow(clippy::too_many_arguments)]
    #[inline]
    pub fn stage_multi_plane(
        &mut self, dmabuf_fds: impl IntoIterator<Item = OwnedFd>, width: u32, height: u32, plane_strides: &[u32],
        plane_offsets: &[u32], fourcc: u32, modifier: u64,
    ) {
        let planes: Box<_> = dmabuf_fds
            .into_iter()
            .zip(plane_strides.iter().copied())
            .zip(plane_offsets.iter().copied())
            .map(|((fd, stride), offset)| PlaneDesc { fd, stride, offset })
            .collect();
        mks_trace!(
            "DMABUF prepared (multi-plane): {width}x{height}, planes={}; import deferred until UpdateDMABUF",
            planes.len()
        );
        self.stage_pending(planes, width, height, fourcc, modifier);
    }

    /// Commits a staged DMABUF update and (re)builds the presentation texture.
    ///
    /// Behavior:
    /// - If `pending` exists: treat this as a page flip, promote `pending` to `active`, and rebuild from the new buffer
    ///   (`old_texture = None`).
    /// - If `pending` does not exist: treat this as an in-place update on the current `active` buffer and forward
    ///   incoming `damage` for partial reuse.
    ///
    /// Returns:
    /// - `Ok(true)`: texture is ready to present.
    /// - `Ok(false)`: no visible work was needed (e.g. invalid/empty damage rect or no active/pending state).
    /// - `Err(..)`: texture rebuild failed.
    #[inline]
    pub fn commit_update(&mut self, x: i32, y: i32, damage_width: i32, damage_height: i32) -> Result<bool, Error> {
        let is_page_flip = if let Some(pending) = self.pending.take() {
            self.active = Some(pending);
            true
        } else {
            false
        };
        let Some(active) = self.active.as_ref() else {
            mks_trace!("UpdateDMABUF commit skipped: no active frame and no pending frame");
            return Ok(false);
        };
        let damage = if is_page_flip {
            mks_trace!("DMABUF commit: page flip detected; rebuilding from pending frame");
            None
        } else {
            if x < 0 || y < 0 || damage_width <= 0 || damage_height <= 0 {
                mks_trace!("UpdateDMABUF commit skipped: invalid damage rect ({x},{y} {damage_width}x{damage_height})",);
                return Ok(false);
            }
            let damage = Damage { x: x as u32, y: y as u32, width: damage_width as u32, height: damage_height as u32 };
            mks_trace!("DMABUF commit: in-place update (trust QEMU damage)={damage:?}");
            Some(damage)
        };
        let gdk_planes: Box<[_]> = active
            .planes
            .iter()
            .map(|plane| DmabufPlane { fd: plane.fd.as_raw_fd(), stride: plane.stride, offset: plane.offset })
            .collect();
        let sanitized = sanitize_opaque_fourcc(active.fourcc).unwrap_or_else(|raw| {
            mks_debug!("FourCC not in opaque-sanitization allowlist; keeping original format");
            raw
        });
        let old_texture = if is_page_flip { None } else { self.texture.as_ref() };
        self.texture = Some(build_dmabuf_texture_planar(
            active.width,
            active.height,
            sanitized,
            active.modifier,
            &gdk_planes,
            old_texture,
            damage,
        )?);
        Ok(true)
    }

    /// Returns the current presentation texture, if available.
    #[inline]
    pub const fn texture(&self) -> Option<&Texture> { self.texture.as_ref() }

    /// Returns the active frame resolution.
    ///
    /// Tuple fields:
    /// - `0`: width in pixels.
    /// - `1`: height in pixels.
    #[inline]
    pub fn resolution(&self) -> (u32, u32) {
        self.active.as_ref().map(|active| (active.width, active.height)).unwrap_or_default()
    }
}
