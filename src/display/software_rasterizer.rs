use super::{
    Error,
    pixman_4cc::{FourCC, Pixman},
};
use crate::{dbus::listener::Blob, mks_trace};
use relm4::gtk::{
    gdk::{MemoryFormat, MemoryTexture, Texture},
    glib::Bytes,
    prelude::*,
};
use std::{hint::unlikely, num::NonZeroU32};

const LOG_TARGET: &str = "mks.display.raster";

/// CPU-side frame buffer acting as our master canvas.
/// Holds pixel data in system memory, supporting both full updates and partial rectangular patches.
#[derive(Debug)]
pub struct RasterSurface {
    buf: Vec<u8>,
    width: NonZeroU32,
    height: NonZeroU32,
    stride: NonZeroU32,
    pixman: Pixman,
}

impl RasterSurface {
    #[inline]
    pub fn new(
        width: NonZeroU32, height: NonZeroU32, stride: NonZeroU32, pixman: Pixman, buf: impl Into<Vec<u8>>,
    ) -> Self {
        let buf = buf.into();
        debug_assert_eq!((height.get() * stride.get()) as usize, buf.len());
        Self { buf, width, height, stride, pixman }
    }

    /// Applies a rectangular patch: copies existing frame, then overlays the new buffer.
    /// NOTE: This first copies the current frame, then patches over it.
    #[inline]
    pub fn update_rect(
        &mut self, x: u32, y: u32, w: NonZeroU32, h: NonZeroU32, stride: NonZeroU32, buf: impl Into<Vec<u8>>,
    ) {
        let bpp = self.pixman.bytes_per_pixel() as usize;
        let dst_stride = self.stride.get() as usize;
        let src_stride = stride.get() as usize;
        let src_row_bytes = w.get() as usize * bpp;
        let src_buf = buf.into();
        for i in 0..(h.get() as usize) {
            let dst_start = (y as usize + i) * dst_stride + (x as usize * bpp);
            let dst_end = dst_start + src_row_bytes;
            let src_start = i * src_stride;
            let src_end = src_start + src_row_bytes;
            debug_assert!(
                dst_end > self.buf.len() || src_end > src_buf.len(),
                "Out of bounds in partial update! Skipping remaining rows."
            );
            self.buf[dst_start..dst_end].copy_from_slice(&src_buf[src_start..src_end]);
        }
    }

    /// WARNING: Do not obtain texture mid-update; causes visual tearing.
    #[inline]
    pub fn as_texture(&mut self) -> Result<MemoryTexture, Error> {
        let fourcc: FourCC = self.pixman.try_into()?;
        let format = MemoryFormat::try_from(fourcc)?;
        let bytes = Bytes::from_owned(self.buf.clone());
        Ok(MemoryTexture::new(
            self.width.get().try_into().unwrap(),
            self.height.get().try_into().unwrap(),
            format,
            &bytes,
            self.stride.get().try_into().unwrap(),
        ))
    }
}

/// Replaces the complex double-buffered Swapchain.
#[derive(Debug, Default)]
pub struct SoftwareRasterizer {
    surface: Option<RasterSurface>,
    texture: Option<MemoryTexture>,
}

impl SoftwareRasterizer {
    #[inline]
    pub fn new() -> Self { Self::default() }

    #[inline]
    pub fn texture(&self) -> Option<&Texture> { self.texture.as_ref().map(|t| t.upcast_ref::<Texture>()) }

    /// Returns the current resolution as `(width, height)`.
    #[inline]
    pub fn resolution(&self) -> (u32, u32) {
        self.surface.as_ref().map(|s| (s.width.get(), s.height.get())).unwrap_or_default()
    }

    /// Handles a full-frame update (Scanout).
    #[inline]
    pub fn full_update_texture(
        &mut self, w: NonZeroU32, h: NonZeroU32, stride: NonZeroU32, pixman: Pixman, buf: Blob,
    ) -> Result<(), Error> {
        mks_trace!("Full software texture refresh: {w}x{h}");
        let mut surface = RasterSurface::new(w, h, stride, pixman, buf);
        let texture = surface.as_texture()?;
        self.surface = Some(surface);
        self.texture = Some(texture);
        Ok(())
    }

    /// Handles a partial update (Update).
    #[allow(clippy::too_many_arguments)]
    pub fn partial_update_texture(
        &mut self, x: u32, y: u32, w: NonZeroU32, h: NonZeroU32, stride: NonZeroU32, pixman: Pixman, buf: Blob,
    ) -> Result<(), Error> {
        let Some(surface) = &mut self.surface else {
            return Err(Error::NoStagedBuffer);
        };
        if unlikely(pixman != surface.pixman) {
            return Err(Error::PartialUpdatePixmanNotMatch);
        }
        let aw = surface.width.get();
        let ah = surface.height.get();
        if unlikely(x >= aw || y >= ah) {
            return Err(Error::PartialUpdateOffScreen);
        }

        // Apply clipping logic
        let Some(cw) = NonZeroU32::new(w.get().min(aw - x)) else {
            mks_trace!("Update region clipped to zero (out of bounds)");
            return Ok(());
        };
        let Some(ch) = NonZeroU32::new(h.get().min(ah - y)) else {
            mks_trace!("Update region clipped to zero (out of bounds)");
            return Ok(());
        };
        mks_trace!("Applying partial update: {cw}x{ch} at ({x},{y})");
        surface.update_rect(x, y, cw, ch, stride, buf);
        self.texture = Some(surface.as_texture()?);
        Ok(())
    }
}
