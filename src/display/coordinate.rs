use std::{cell::Cell, num::NonZeroU32};

/// Cached viewport transform from VM space to widget space.
#[derive(Debug, Clone, Copy)]
pub struct Viewport {
    /// Widget logical pixels per VM pixel.
    pub scale: f32,
    /// Left offset in widget logical coordinates.
    pub offset_x: f32,
    /// Top offset in widget logical coordinates.
    pub offset_y: f32,
}

/// Maps coordinates across VM pixels, GTK logical widget space, and physical canvas space.
///
/// Transform chain: `VM -> Widget (contain scale + offset) -> Physical (* ui_scale)`.
/// The viewport step follows CSS `object-fit: contain` semantics and is cached.
#[derive(Debug, Clone)]
pub struct Coordinate {
    pub vm_resolution: (u32, u32),
    widget_size_logical: (f32, f32),
    pub ui_scale: f32,
    cached_viewport: Cell<Option<Viewport>>,
    transform_dirty: Cell<bool>,
}

impl Coordinate {
    pub fn new(vm_w: u32, vm_h: u32, widget_w: f32, widget_h: f32, ui_scale: f32) -> Self {
        Self {
            vm_resolution: (vm_w, vm_h),
            widget_size_logical: (widget_w, widget_h),
            ui_scale,
            cached_viewport: None.into(),
            transform_dirty: true.into(),
        }
    }

    #[inline]
    pub fn set_vm_resolution(&mut self, w: u32, h: u32) {
        self.vm_resolution = (w, h);
        self.transform_dirty.set(true);
    }

    #[inline]
    pub fn set_widget_size(&mut self, w: f32, h: f32) {
        self.widget_size_logical = (w, h);
        self.transform_dirty.set(true);
    }

    /// Returns physical canvas size as `(physical_width, physical_height)`.
    /// Returns `None` if widget size or `ui_scale` is non-positive or non-finite.
    #[inline]
    pub fn physical_canvas_size(&self) -> Option<(NonZeroU32, NonZeroU32)> {
        let (w, h) = self.widget_size_logical;
        let scale = self.ui_scale;
        if w <= 0. || !w.is_finite() {
            return None;
        }
        if h <= 0. || !h.is_finite() {
            return None;
        }
        if scale <= 0. || !scale.is_finite() {
            return None;
        }
        let phys_w = (w * scale).max(1.) as u32;
        let phys_h = (h * scale).max(1.) as u32;
        Some((NonZeroU32::new(phys_w)?, NonZeroU32::new(phys_h)?))
    }

    /// Computes the viewport transform using CSS `object-fit: contain` semantics.
    /// Returns `None` if VM/widget dimensions are invalid.
    #[inline]
    pub const fn calculate_contain_transform(&self) -> Option<Viewport> {
        let (vm_w, vm_h) = self.vm_resolution;
        let (widget_w, widget_h) = self.widget_size_logical;
        if vm_w == 0 || vm_h == 0 {
            return None;
        }
        if widget_h <= 0. || !widget_h.is_finite() {
            return None;
        }
        if widget_w <= 0. || !widget_w.is_finite() {
            return None;
        }
        let vm_w = vm_w as f32;
        let vm_h = vm_h as f32;
        if widget_w * vm_h < widget_h * vm_w {
            let scale = widget_w / vm_w;
            let offset_x = 0.;
            let offset_y = (widget_h - vm_h * scale) / 2.;
            Some(Viewport { scale, offset_x, offset_y })
        } else {
            let scale = widget_h / vm_h;
            let offset_x = (widget_w - vm_w * scale) / 2.;
            let offset_y = 0.;
            Some(Viewport { scale, offset_x, offset_y })
        }
    }

    /// Returns the viewport transform; recomputes when the cache is dirty.
    #[inline]
    pub fn get_cached_viewport(&self) -> Option<Viewport> {
        if self.transform_dirty.get() {
            self.cached_viewport.set(self.calculate_contain_transform());
            self.transform_dirty.set(false);
        }
        self.cached_viewport.get()
    }

    /// Maps a widget logical point to VM coordinates, clamped to VM bounds.
    /// Returns `(guest_x, guest_y)`.
    #[inline]
    pub fn widget_to_guest(&self, logical_x: f32, logical_y: f32) -> Option<(u32, u32)> {
        let (vm_w, vm_h) = self.vm_resolution;
        if vm_w == 0 || vm_h == 0 {
            return None;
        }
        let viewport = self.get_cached_viewport()?;
        let guest_x = ((logical_x - viewport.offset_x) / viewport.scale).round().clamp(0., (vm_w - 1) as f32);
        let guest_y = ((logical_y - viewport.offset_y) / viewport.scale).round().clamp(0., (vm_h - 1) as f32);
        Some((guest_x as u32, guest_y as u32))
    }

    /// Returns the VM display rect in widget space as `(left, top, width, height)`.
    #[inline]
    pub fn vm_display_bounds(&self) -> Option<(f32, f32, f32, f32)> {
        let viewport = self.get_cached_viewport()?;
        let (vm_w, vm_h) = self.vm_resolution;
        Some((viewport.offset_x, viewport.offset_y, vm_w as f32 * viewport.scale, vm_h as f32 * viewport.scale))
    }

    /// Checks whether a widget-space point lies inside the VM display rect.
    #[inline]
    pub fn is_in_viewport(&self, px: f32, py: f32) -> bool {
        let Some((x, y, w, h)) = self.vm_display_bounds() else {
            return false;
        };
        px >= x && px <= (x + w) && py >= y && py <= (y + h)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPSILON: f32 = 1e-4;

    /// Helper to assert fuzzy float equality for coordinates.
    /// Allows ±1 pixel tolerance for rounding errors at boundaries.
    fn assert_coord_eq(actual: (u32, u32), expected: (u32, u32)) {
        let (ax, ay) = actual;
        let (ex, ey) = expected;
        assert!(ax.abs_diff(ex) <= 1, "X coordinate mismatch: actual {}, expected {}", ax, ex);
        assert!(ay.abs_diff(ey) <= 1, "Y coordinate mismatch: actual {}, expected {}", ay, ey);
    }

    fn assert_f32_close(actual: f32, expected: f32) {
        assert!(
            (actual - expected).abs() <= EPSILON,
            "float mismatch: actual={actual}, expected={expected}, diff={}",
            (actual - expected).abs()
        );
    }

    /// Tests perfect 1:1 aspect ratio match (no scaling needed).
    /// This catches bugs where we accidentally invert the scaling direction.
    #[test]
    fn test_perfect_fit_mapping() {
        // VM: 1920x1080, Widget: 1920x1080 → scale=1.0, offsets=(0,0)
        let coord = Coordinate::new(1920, 1080, 1920.0, 1080.0, 1.0);

        // Top-left corner should map exactly
        assert_coord_eq(coord.widget_to_guest(0.0, 0.0).unwrap(), (0, 0));
        // Center of widget maps to center of VM
        assert_coord_eq(coord.widget_to_guest(960.0, 540.0).unwrap(), (960, 540));
        // Bottom-right corner (1919, not 1920, since 0-indexed)
        assert_coord_eq(coord.widget_to_guest(1919.0, 1079.0).unwrap(), (1919, 1079));
    }

    /// Tests pillarbox scenario (black bars on left/right).
    /// Widget is wider than VM aspect ratio, so we scale to fit height
    /// and center horizontally with black bars on sides.
    /// This verifies offset_x calculation and clamping to VM bounds.
    #[test]
    fn test_pillarbox_mapping() {
        // VM: 800x600 (4:3), Widget: 1600x900 (16:9)
        // Scale = 900/600 = 1.5, VM width on screen = 1200
        // Black bars: (1600-1200)/2 = 200px each side
        let coord = Coordinate::new(800, 600, 1600.0, 900.0, 1.0);

        // Click in left black bar → should clamp to VM left edge
        assert_coord_eq(coord.widget_to_guest(100.0, 450.0).unwrap(), (0, 300));

        // Click exactly on left VM edge (200px offset)
        assert_coord_eq(coord.widget_to_guest(200.0, 0.0).unwrap(), (0, 0));

        // Click on center → should map to VM center
        assert_coord_eq(coord.widget_to_guest(800.0, 450.0).unwrap(), (400, 300));

        // Click exactly on right VM edge (1600-200=1400px)
        assert_coord_eq(coord.widget_to_guest(1400.0, 900.0).unwrap(), (799, 599));
    }

    /// Tests letterbox scenario (black bars on top/bottom).
    /// Widget is taller than VM aspect ratio, so we scale to fit width
    /// and center vertically with black bars on top/bottom.
    /// This verifies offset_y calculation.
    #[test]
    fn test_letterbox_mapping() {
        // VM: 1920x1080 (16:9), Widget: 1000x1000 (1:1)
        // Scale = 1000/1920 ≈ 0.5208, VM height on screen ≈ 562.5
        // Black bars: (1000-562.5)/2 ≈ 218.75px top/bottom
        let coord = Coordinate::new(1920, 1080, 1000.0, 1000.0, 1.0);

        // Center check is most reliable for letterbox
        assert_coord_eq(coord.widget_to_guest(500.0, 500.0).unwrap(), (960, 540));
    }

    /// Tests that cached transform is invalidated when VM resolution changes.
    /// This catches bugs where we forget to dirty the cache, causing
    /// stale transforms to be used.
    #[test]
    fn test_cache_invalidation() {
        let mut coord = Coordinate::new(100, 100, 200.0, 200.0, 1.0);

        // Initial state: Scale = 200/100 = 2.0
        assert_coord_eq(coord.widget_to_guest(100.0, 100.0).unwrap(), (50, 50));

        // Change VM resolution → should invalidate cache and recalculate
        coord.set_vm_resolution(200, 200);
        // Now scale = 200/200 = 1.0
        assert_coord_eq(coord.widget_to_guest(100.0, 100.0).unwrap(), (100, 100));

        // Change widget size → should invalidate cache again
        coord.set_widget_size(400.0, 400.0);
        // Now scale = 400/200 = 2.0
        assert_coord_eq(coord.widget_to_guest(200.0, 200.0).unwrap(), (100, 100));
    }

    /// Tests zero-size handling to prevent division by zero.
    /// This ensures safety checks are in place for edge cases.
    #[test]
    fn test_zero_size_handling() {
        // Zero widget size
        let coord = Coordinate::new(1920, 1080, 0.0, 0.0, 1.0);
        assert!(coord.widget_to_guest(10.0, 10.0).is_none());

        // Zero VM resolution
        let coord2 = Coordinate::new(0, 0, 100.0, 100.0, 1.0);
        assert!(coord2.widget_to_guest(10.0, 10.0).is_none());
    }

    #[test]
    fn test_calculate_contain_transform_fit_width_branch() {
        let coord = Coordinate::new(1920, 1080, 1000.0, 1000.0, 1.0);
        let viewport = coord.calculate_contain_transform().unwrap();

        assert_f32_close(viewport.scale, 1000.0 / 1920.0);
        assert_f32_close(viewport.offset_x, 0.0);
        assert_f32_close(viewport.offset_y, 218.75);
    }

    #[test]
    fn test_calculate_contain_transform_fit_height_branch() {
        let coord = Coordinate::new(800, 600, 1600.0, 900.0, 1.0);
        let viewport = coord.calculate_contain_transform().unwrap();

        assert_f32_close(viewport.scale, 1.5);
        assert_f32_close(viewport.offset_x, 200.0);
        assert_f32_close(viewport.offset_y, 0.0);
    }

    #[test]
    fn test_vm_display_bounds_matches_viewport_math() {
        let coord = Coordinate::new(800, 600, 1600.0, 900.0, 1.0);
        let (x, y, w, h) = coord.vm_display_bounds().unwrap();

        assert_f32_close(x, 200.0);
        assert_f32_close(y, 0.0);
        assert_f32_close(w, 1200.0);
        assert_f32_close(h, 900.0);
    }

    #[test]
    fn test_is_in_viewport_contract_points() {
        let coord = Coordinate::new(800, 600, 1600.0, 900.0, 1.0);

        // Integration contract for vm_display capture gating.
        assert!(coord.is_in_viewport(800.0, 450.0));
        assert!(!coord.is_in_viewport(100.0, 450.0));
        assert!(!coord.is_in_viewport(1500.0, 450.0));
    }

    #[test]
    fn test_is_in_viewport_boundary_semantics() {
        let coord = Coordinate::new(800, 600, 1600.0, 900.0, 1.0);

        // Lock current graphene boundary semantics used by capture transitions.
        assert!(coord.is_in_viewport(200.0, 0.0));
        assert!(coord.is_in_viewport(1399.999, 899.999));
        assert!(!coord.is_in_viewport(1400.001, 450.0));
        assert!(!coord.is_in_viewport(800.0, 900.001));
    }

    #[test]
    fn test_physical_canvas_size_matrix() {
        let coord = Coordinate::new(1920, 1080, 100.25, 50.75, 1.5);
        let (w, h) = coord.physical_canvas_size().unwrap();
        assert_eq!(w.get(), 150);
        assert_eq!(h.get(), 76);

        let mut invalid = Coordinate::new(1920, 1080, 100.0, 50.0, 1.0);

        invalid.set_widget_size(0.0, 50.0);
        assert!(invalid.physical_canvas_size().is_none());
        invalid.set_widget_size(-1.0, 50.0);
        assert!(invalid.physical_canvas_size().is_none());
        invalid.set_widget_size(f32::NAN, 50.0);
        assert!(invalid.physical_canvas_size().is_none());
        invalid.set_widget_size(f32::INFINITY, 50.0);
        assert!(invalid.physical_canvas_size().is_none());

        invalid.set_widget_size(100.0, 0.0);
        assert!(invalid.physical_canvas_size().is_none());
        invalid.set_widget_size(100.0, -1.0);
        assert!(invalid.physical_canvas_size().is_none());
        invalid.set_widget_size(100.0, f32::NAN);
        assert!(invalid.physical_canvas_size().is_none());
        invalid.set_widget_size(100.0, f32::NEG_INFINITY);
        assert!(invalid.physical_canvas_size().is_none());

        invalid.set_widget_size(100.0, 50.0);
        invalid.ui_scale = 0.0;
        assert!(invalid.physical_canvas_size().is_none());
        invalid.ui_scale = -1.0;
        assert!(invalid.physical_canvas_size().is_none());
        invalid.ui_scale = f32::NAN;
        assert!(invalid.physical_canvas_size().is_none());
        invalid.ui_scale = f32::INFINITY;
        assert!(invalid.physical_canvas_size().is_none());
    }

    #[test]
    fn test_widget_to_guest_clamps_extremes() {
        let coord = Coordinate::new(800, 600, 1600.0, 900.0, 1.0);

        assert_eq!(coord.widget_to_guest(-10_000.0, -10_000.0).unwrap(), (0, 0));
        assert_eq!(coord.widget_to_guest(10_000.0, 10_000.0).unwrap(), (799, 599));
        assert_eq!(coord.widget_to_guest(-1000.0, 450.0).unwrap(), (0, 300));
        assert_eq!(coord.widget_to_guest(800.0, -1000.0).unwrap(), (400, 0));
    }

    #[test]
    fn test_widget_to_guest_invalid_float_behavior_is_stable() {
        let coord = Coordinate::new(100, 100, 100.0, 100.0, 1.0);

        // Current behavior lock: NaN coordinates clamp/cast to zero.
        assert_eq!(coord.widget_to_guest(f32::NAN, 10.0).unwrap(), (0, 10));
        assert_eq!(coord.widget_to_guest(10.0, f32::NAN).unwrap(), (10, 0));

        // Current behavior lock: infinities saturate to bounds.
        assert_eq!(coord.widget_to_guest(f32::INFINITY, 10.0).unwrap(), (99, 10));
        assert_eq!(coord.widget_to_guest(10.0, f32::INFINITY).unwrap(), (10, 99));
        assert_eq!(coord.widget_to_guest(f32::NEG_INFINITY, 10.0).unwrap(), (0, 10));
        assert_eq!(coord.widget_to_guest(10.0, f32::NEG_INFINITY).unwrap(), (10, 0));
    }

    #[test]
    fn test_cached_viewport_consistency_and_recompute() {
        let mut coord = Coordinate::new(800, 600, 1600.0, 900.0, 1.0);
        let first = coord.get_cached_viewport().unwrap();
        let second = coord.get_cached_viewport().unwrap();

        assert_f32_close(first.scale, second.scale);
        assert_f32_close(first.offset_x, second.offset_x);
        assert_f32_close(first.offset_y, second.offset_y);

        coord.set_vm_resolution(1920, 1080);
        let after_vm_resize = coord.get_cached_viewport().unwrap();
        assert_f32_close(after_vm_resize.scale, 1600.0 / 1920.0);
        assert_f32_close(after_vm_resize.offset_x, 0.0);

        coord.set_widget_size(1920.0, 1080.0);
        let after_widget_resize = coord.get_cached_viewport().unwrap();
        assert_f32_close(after_widget_resize.scale, 1.0);
        assert_f32_close(after_widget_resize.offset_x, 0.0);
        assert_f32_close(after_widget_resize.offset_y, 0.0);
    }

    #[test]
    fn test_confined_click_mapping_contract() {
        let coord = Coordinate::new(800, 600, 1600.0, 900.0, 1.0);

        // Same contract vm_display SetConfined path relies on.
        assert_eq!(coord.widget_to_guest(100.0, 450.0).unwrap(), (0, 300));
        assert_eq!(coord.widget_to_guest(800.0, 450.0).unwrap(), (400, 300));
        assert_eq!(coord.widget_to_guest(1500.0, 450.0).unwrap(), (799, 300));
        assert_eq!(coord.widget_to_guest(1400.0, 899.0).unwrap(), (799, 599));
    }

    #[test]
    fn test_resize_contract_physical_canvas_size() {
        let regular = Coordinate::new(1920, 1080, 800.0, 600.0, 1.0);
        let (rw, rh) = regular.physical_canvas_size().unwrap();
        assert_eq!(rw.get(), 800);
        assert_eq!(rh.get(), 600);

        let hidpi = Coordinate::new(1920, 1080, 800.0, 600.0, 1.75);
        let (hw, hh) = hidpi.physical_canvas_size().unwrap();
        assert_eq!(hw.get(), 1400);
        assert_eq!(hh.get(), 1050);
    }
}
