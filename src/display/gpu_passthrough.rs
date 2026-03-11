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
use arrayvec::ArrayVec;
use relm4::gtk::gdk::Texture;
use std::{
    num::NonZeroU32,
    os::fd::{AsRawFd, OwnedFd},
};

const LOG_TARGET: &str = "mks.display.gpu_passthrough";

#[derive(Debug)]
struct PlaneDesc {
    fd: OwnedFd,
    stride: NonZeroU32,
    offset: u32,
}

const MAX_PLANES: usize = 4;

#[derive(Debug)]
struct DmabufState {
    planes: ArrayVec<PlaneDesc, MAX_PLANES>,
    fourcc: FourCC,
    modifier: u64,
    width: NonZeroU32,
    height: NonZeroU32,
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
    pub fn new() -> Self { Self::default() }

    #[inline]
    fn stage_pending(
        &mut self, planes: impl IntoIterator<Item = PlaneDesc>, w: NonZeroU32, h: NonZeroU32, fourcc: FourCC,
        modifier: u64,
    ) {
        let planes = planes.into_iter().collect();
        self.pending = DmabufState { planes, fourcc, modifier, width: w, height: h }.into();
    }

    /// Stages a single-plane scanout frame into `pending`.
    ///
    /// Any previous not-yet-committed `pending` frame is replaced.
    #[inline]
    pub fn stage_single_plane(
        &mut self, dmabuf_fd: OwnedFd, w: NonZeroU32, h: NonZeroU32, stride: NonZeroU32, fourcc: FourCC, modifier: u64,
    ) {
        let planes = [PlaneDesc { fd: dmabuf_fd, stride, offset: 0 }];
        self.stage_pending(planes, w, h, fourcc, modifier);
        mks_trace!("DMABUF prepared (single-plane): {w}x{h}; import deferred until UpdateDMABUF");
    }

    /// Stages a multi-plane scanout frame into `pending`.
    ///
    /// FDs/strides/offsets are paired by plane index; any previous `pending`
    /// frame is replaced.
    #[allow(clippy::too_many_arguments)]
    #[inline]
    pub fn stage_multi_plane(
        &mut self, dmabuf_fds: impl IntoIterator<Item = OwnedFd>, w: NonZeroU32, h: NonZeroU32, strides: &[NonZeroU32],
        offsets: &[u32], fourcc: FourCC, modifier: u64,
    ) {
        let planes = dmabuf_fds
            .into_iter()
            .take(MAX_PLANES)
            .zip(strides.iter().copied())
            .zip(offsets.iter().copied())
            .map(|((fd, stride), offset)| PlaneDesc { fd, stride, offset })
            .collect::<ArrayVec<_, MAX_PLANES>>();
        mks_trace!(
            "DMABUF prepared (multi-plane): {w}x{h}, planes={}; import deferred until UpdateDMABUF",
            planes.len()
        );
        self.stage_pending(planes, w, h, fourcc, modifier);
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
    pub fn commit_update(&mut self, x: u32, y: u32, w: NonZeroU32, h: NonZeroU32) -> Result<bool, Error> {
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
        let (damage, old_texture) = if is_page_flip {
            mks_trace!("DMABUF commit: page flip detected; rebuilding from pending frame");
            (None, None)
        } else {
            let damage = Damage { x, y, width: w, height: h };
            mks_trace!("DMABUF commit: in-place update (trust QEMU damage)={damage:?}");
            (Some(damage), self.texture())
        };
        let gdk_planes = active
            .planes
            .iter()
            .map(|&PlaneDesc { ref fd, stride, offset }| DmabufPlane { fd: fd.as_raw_fd(), stride, offset })
            .collect::<ArrayVec<_, MAX_PLANES>>();
        // Convert ARGB -> XRGB to avoid compositing with transparent guest content.
        let sanitized = sanitize_opaque_fourcc(active.fourcc).unwrap_or_else(|raw| {
            mks_debug!("FourCC not in opaque-sanitization allowlist; keeping original format");
            raw
        });
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
    /// - `width`: active frame width in pixels.
    /// - `height`: active frame height in pixels.
    #[inline]
    pub fn resolution(&self) -> (u32, u32) {
        self.active.as_ref().map(|active| (active.width.get(), active.height.get())).unwrap_or_default()
    }
}
