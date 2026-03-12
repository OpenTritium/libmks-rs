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
    dmabuf::{Damage, DmabufPlane, build_dmabuf_texture_planar},
    pixman_4cc::{FourCC, sanitize_opaque_fourcc},
};
use crate::{display::crop::CropInfo, mks_debug, mks_trace, mks_warn};
use arrayvec::ArrayVec;
use relm4::gtk::gdk::Texture;
use std::{
    num::NonZeroU32,
    os::fd::{AsRawFd, OwnedFd},
};

const LOG_TARGET: &str = "mks.display.gpu_passthrough";
const MAX_PLANES: usize = 4;

/// Plane descriptor for DMA-BUF file descriptor + layout metadata.
#[derive(Debug)]
struct PlaneDesc {
    fd: OwnedFd,
    /// DMA-BUF file descriptor (transferred to GDK/EGL)
    stride: NonZeroU32,
    /// Row stride in bytes
    offset: u32, // Byte offset to plane data
}

/// Normalized DMA-BUF state (V1/V2 -> internal model).
#[derive(Debug)]
struct DmabufState {
    planes: ArrayVec<PlaneDesc, MAX_PLANES>,
    sanitized_fourcc: FourCC,
    /// ARGB->XRGB pre-sanitized; cached
    modifier: u64,
    /// DRM modifier (linear, tiled, etc.)
    backing_width: NonZeroU32,
    /// Physical buffer width (for Texture creation)
    backing_height: NonZeroU32,
    /// Physical buffer height
    crop_x: u32,
    /// Viewport offset X within backing
    crop_y: u32,
    /// Viewport offset Y within backing
    crop_width: NonZeroU32,
    /// Viewport width
    crop_height: NonZeroU32, // Viewport height
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

    /// Stage V1 single-plane DMA-BUF (backing = crop).
    #[inline]
    pub fn stage_single_plane(
        &mut self, dmabuf_fd: OwnedFd, w: NonZeroU32, h: NonZeroU32, stride: NonZeroU32, fourcc: FourCC, modifier: u64,
    ) {
        let planes = [PlaneDesc { fd: dmabuf_fd, stride, offset: 0 }].into_iter().collect();
        // Pre-sanitize FourCC to avoid repeated conversion on every commit.
        let sanitized_fourcc = sanitize_opaque_fourcc(fourcc).unwrap_or_else(|raw| {
            mks_debug!("FourCC not in opaque-sanitization allowlist; keeping original format");
            raw
        });
        self.pending = Some(DmabufState {
            planes,
            sanitized_fourcc,
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

    /// Stage V2 multi-plane DMA-BUF (separate backing and crop).
    #[allow(clippy::too_many_arguments)]
    #[inline]
    pub fn stage_multi_plane(
        &mut self, dmabuf_fds: impl IntoIterator<Item = OwnedFd>, crop_x: u32, crop_y: u32, crop_w: NonZeroU32,
        crop_h: NonZeroU32, backing_w: NonZeroU32, backing_h: NonZeroU32, strides: &[NonZeroU32], offsets: &[u32],
        fourcc: FourCC, modifier: u64,
    ) {
        // Validate crop coordinates are within backing buffer bounds.
        debug_assert!(
            crop_x.saturating_add(crop_w.get()) <= backing_w.get(),
            "crop_x({crop_x}) + crop_w({crop_w}) exceeds backing_w({backing_w})"
        );
        debug_assert!(
            crop_y.saturating_add(crop_h.get()) <= backing_h.get(),
            "crop_y({crop_y}) + crop_h({crop_h}) exceeds backing_h({backing_h})"
        );
        let planes = dmabuf_fds
            .into_iter()
            .take(MAX_PLANES)
            .zip(strides.iter().copied())
            .zip(offsets.iter().copied())
            .map(|((fd, stride), offset)| PlaneDesc { fd, stride, offset })
            .collect::<ArrayVec<_, MAX_PLANES>>();
        // Pre-sanitize FourCC to avoid repeated conversion on every commit.
        let sanitized_fourcc = sanitize_opaque_fourcc(fourcc).unwrap_or_else(|raw| {
            mks_debug!("FourCC not in opaque-sanitization allowlist; keeping original format");
            raw
        });
        self.pending = Some(DmabufState {
            planes,
            sanitized_fourcc,
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

    /// Commit staged DMABUF: page flip if `pending` exists, otherwise in-place update with damage.
    ///
    /// Damage (x,y) from QEMU is relative to viewport; we map to backing coords by adding crop offsets.
    ///
    /// Returns: `Ok(true)` = texture ready, `Ok(false)` = no work needed, `Err(..)` = rebuild failed.
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
            let damage_x = x.saturating_add(active.crop_x);
            let damage_y = y.saturating_add(active.crop_y);
            let damage_w = w.get();
            let damage_h = h.get();

            // Defensive: verify mapped damage is within backing buffer bounds.
            if damage_x.saturating_add(damage_w) > active.backing_width.get()
                || damage_y.saturating_add(damage_h) > active.backing_height.get()
            {
                mks_warn!(
                    "DMABUF commit: mapped damage rect ({damage_x},{damage_y} {damage_w}x{damage_h}) exceeds backing \
                     buffer ({}x{})",
                    active.backing_width.get(),
                    active.backing_height.get()
                );
                return Ok(false);
            }
            // Position mapped to backing coords; dimensions unchanged (same in both spaces).
            let damage = Damage { x: damage_x, y: damage_y, width: w, height: h };
            mks_trace!(
                "DMABUF commit: in-place update. Damage rect (backing)={damage_x},{damage_y} {damage_w}x{damage_h}"
            );
            (Some(damage), self.texture())
        };
        let gdk_planes = active
            .planes
            .iter()
            .map(|&PlaneDesc { ref fd, stride, offset }| DmabufPlane { fd: fd.as_raw_fd(), stride, offset })
            .collect::<ArrayVec<_, MAX_PLANES>>();

        // Build Texture using backing dimensions (not crop dimensions) to satisfy EGL stride checks.
        // Uses pre-sanitized FourCC from staging (cached to avoid repeated ARGB->XRGB conversion).
        self.texture = Some(build_dmabuf_texture_planar(
            active.backing_width,
            active.backing_height,
            active.sanitized_fourcc,
            active.modifier,
            &gdk_planes,
            old_texture,
            damage,
        )?);

        Ok(true)
    }

    /// Presentation texture (backing buffer dimensions). Use [`crop_info()`] for viewport.
    #[inline]
    pub const fn texture(&self) -> Option<&Texture> { self.texture.as_ref() }

    /// Viewport (crop) info for UI clipping/translation.
    #[inline]
    pub fn crop_info(&self) -> Option<CropInfo> {
        self.active.as_ref().map(|a| CropInfo {
            x: a.crop_x as f32,
            y: a.crop_y as f32,
            width: a.crop_width.get() as f32,
            height: a.crop_height.get() as f32,
        })
    }

    /// Returns guest visible resolution: (width, height).
    ///
    /// Used for window/canvas size calculations.
    #[inline]
    pub fn resolution(&self) -> (u32, u32) {
        self.active.as_ref().map(|a| (a.crop_width.get(), a.crop_height.get())).unwrap_or_default()
    }
}
