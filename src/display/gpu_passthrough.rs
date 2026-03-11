//! DMA-BUF GPU passthrough state machine.
//!
//! This module follows a strict `Prepare (Scanout*) -> Commit (UpdateDMABUF)`
//! flow:
//! - `Scanout*` only stages the latest incoming buffers into `pending`.
//! - `UpdateDMABUF` promotes `pending` to `active` and performs the actual texture import/update.
//!
//! # Normalization Strategy
//! Both V1 (single-plane, no backing concept) and V2 (multi-plane, separate backing and crop)
//! protocols are normalized at the `stage_*` entry points. The internal state always maintains:
//! - `backing_width/height`: physical buffer dimensions (for Texture creation)
//! - `crop_x/y/w/h`: visible viewport (for UI rendering and damage mapping)
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
const MAX_PLANES: usize = 4;

#[derive(Debug)]
struct PlaneDesc {
    fd: OwnedFd,
    stride: NonZeroU32,
    offset: u32,
}

/// Unified DMA-BUF state model (normalizes V1 and V2).
///
/// - `backing_width/height`: physical buffer dimensions (used for GDK/EGL Texture creation)
/// - `crop_x/y/w/h`: visible viewport region (used for UI裁剪和局部刷新对齐)
#[derive(Debug)]
struct DmabufState {
    planes: ArrayVec<PlaneDesc, MAX_PLANES>,
    fourcc: FourCC,
    modifier: u64,
    /// Physical buffer dimensions (for Texture creation stride validation)
    backing_width: NonZeroU32,
    backing_height: NonZeroU32,
    /// Visible viewport region (Scanout's x, y, width, height)
    crop_x: u32,
    crop_y: u32,
    crop_width: NonZeroU32,
    crop_height: NonZeroU32,
}

/// Viewport crop information for UI rendering layer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CropInfo {
    /// Horizontal offset of visible viewport within the backing buffer
    pub x: f32,
    /// Vertical offset of visible viewport within the backing buffer
    pub y: f32,
    /// Width of visible viewport
    pub width: f32,
    /// Height of visible viewport
    pub height: f32,
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

    /// Normalizes V1 protocol (single-plane, no backing concept) to internal model.
    ///
    /// Physical buffer dimensions equal visible viewport dimensions.
    #[inline]
    pub fn stage_single_plane(
        &mut self, dmabuf_fd: OwnedFd, w: NonZeroU32, h: NonZeroU32, stride: NonZeroU32, fourcc: FourCC, modifier: u64,
    ) {
        let planes = [PlaneDesc { fd: dmabuf_fd, stride, offset: 0 }].into_iter().collect();
        self.pending = Some(DmabufState {
            planes,
            fourcc,
            modifier,
            backing_width: w,
            backing_height: h,
            crop_x: 0,
            crop_y: 0,
            crop_width: w,
            crop_height: h,
        });
        mks_trace!("DMABUF prepared (V1 single-plane): {w}x{h}");
    }

    /// Normalizes V2 protocol (multi-plane, separate backing and crop) to internal model.
    ///
    /// - `crop_x/y`: offset of visible viewport within the backing buffer
    /// - `crop_w/h`: visible viewport dimensions
    /// - `backing_w/h`: physical buffer dimensions (may be larger due to GPU alignment)
    #[allow(clippy::too_many_arguments)]
    #[inline]
    pub fn stage_multi_plane(
        &mut self, dmabuf_fds: impl IntoIterator<Item = OwnedFd>, crop_x: u32, crop_y: u32, crop_w: NonZeroU32,
        crop_h: NonZeroU32, backing_w: NonZeroU32, backing_h: NonZeroU32, strides: &[NonZeroU32], offsets: &[u32],
        fourcc: FourCC, modifier: u64,
    ) {
        let planes = dmabuf_fds
            .into_iter()
            .take(MAX_PLANES)
            .zip(strides.iter().copied())
            .zip(offsets.iter().copied())
            .map(|((fd, stride), offset)| PlaneDesc { fd, stride, offset })
            .collect::<ArrayVec<_, MAX_PLANES>>();
        self.pending = Some(DmabufState {
            planes,
            fourcc,
            modifier,
            backing_width: backing_w,
            backing_height: backing_h,
            crop_x,
            crop_y,
            crop_width: crop_w,
            crop_height: crop_h,
        });
        mks_trace!(
            "DMABUF prepared (V2 multi-plane): backing={backing_w}x{backing_h}, crop={crop_w}x{crop_h} at \
             ({crop_x},{crop_y})"
        );
    }

    /// Commits a staged DMABUF update and (re)builds the presentation texture.
    ///
    /// Behavior:
    /// - If `pending` exists: treat this as a page flip, promote `pending` to `active`, and rebuild from the new buffer
    ///   (`old_texture = None`).
    /// - If `pending` does not exist: treat this as an in-place update on the current `active` buffer and forward
    ///   incoming `damage` for partial reuse.
    ///
    /// Note: QEMU's damage coordinates `(x, y)` are relative to the visible viewport (crop).
    /// We map them to backing buffer coordinates by adding `crop_x` and `crop_y` offsets.
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
            // QEMU's damage coordinates are relative to the visible viewport (crop).
            // We must map them to backing buffer coordinates.
            let damage = Damage { x: x + active.crop_x, y: y + active.crop_y, width: w, height: h };
            mks_trace!("DMABUF commit: in-place update. Mapped damage to backing rect={damage:?}");
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

        // Build Texture using backing dimensions (not crop dimensions) to satisfy EGL stride checks.
        self.texture = Some(build_dmabuf_texture_planar(
            active.backing_width,
            active.backing_height,
            sanitized,
            active.modifier,
            &gdk_planes,
            old_texture,
            damage,
        )?);

        Ok(true)
    }

    /// Returns the current presentation texture, if available.
    ///
    /// Note: The texture dimensions match the backing buffer, not the visible viewport.
    /// Use `crop_info()` to get the viewport region for proper UI rendering.
    #[inline]
    pub const fn texture(&self) -> Option<&Texture> { self.texture.as_ref() }

    /// Returns the visible viewport (crop) information for UI rendering.
    ///
    /// UI must use this to clip and translate the backing texture when drawing.
    #[inline]
    pub fn crop_info(&self) -> Option<CropInfo> {
        self.active.as_ref().map(|a| CropInfo {
            x: a.crop_x as f32,
            y: a.crop_y as f32,
            width: a.crop_width.get() as f32,
            height: a.crop_height.get() as f32,
        })
    }

    /// Returns the guest visible resolution (i.e., the crop dimensions).
    ///
    /// Use this for window/canvas size calculations.
    #[inline]
    pub fn visible_resolution(&self) -> (u32, u32) {
        self.active.as_ref().map(|a| (a.crop_width.get(), a.crop_height.get())).unwrap_or_default()
    }

    /// Returns the active frame resolution (visible viewport dimensions).
    ///
    /// This is an alias for `visible_resolution()` for backward compatibility.
    #[inline]
    pub fn resolution(&self) -> (u32, u32) { self.visible_resolution() }
}
