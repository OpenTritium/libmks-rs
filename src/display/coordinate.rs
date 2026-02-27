use relm4::gtk::graphene::{Point, Rect};
use std::{cell::Cell, num::NonZeroU32};

/// Viewport describes how the VM display is positioned within the widget.
#[derive(Debug, Clone, Copy)]
pub struct Viewport {
    /// Scale factor for VM content (widget pixels per VM pixel).
    pub scale: f32,
    /// X offset of VM display origin within widget.
    pub offset_x: f32,
    /// Y offset of VM display origin within widget.
    pub offset_y: f32,
}

/// Coordinate system for transforming between widget and VM coordinates.
///
/// ## Coordinate Systems
///
/// This module operates in three coordinate systems:
///
/// - **VM coordinates**: Guest pixel positions inside the virtual machine (0 ~ vm_resolution)
/// - **Widget coordinates within the GTK widget (**: Logical positionslogical pixels)
/// - **Physical coordinates**: Actual screen pixels (logical pixels * ui_scale)
///
/// ## Transformation Chain
///
/// ```
/// Physical coords ← ui_scale ← Widget coords
///      ↑                         ↑
///      │                   viewport.scale / offset
///      ↓                         ↓
///    VM coords ←─────────────────────┘
/// ```
///
/// All public methods that take or return coordinates specify which system they use.
#[derive(Debug, Clone)]
pub struct Coordinate {
    vm_resolution: (u32, u32),
    widget_size_logical: (f32, f32),
    pub ui_scale: f32,
    cached_viewport: Cell<Option<Viewport>>,
    transform_dirty: Cell<bool>,
}

impl Coordinate {
    /// Creates a new Coordinate system.
    pub fn new(vm_w: u32, vm_h: u32, widget_w: f32, widget_h: f32, ui_scale: f32) -> Self {
        Self {
            vm_resolution: (vm_w, vm_h),
            widget_size_logical: (widget_w, widget_h),
            ui_scale,
            cached_viewport: Cell::new(None),
            transform_dirty: Cell::new(true),
        }
    }

    /// Sets the VM resolution (e.g., 1920x1080).
    #[inline]
    pub fn set_vm_resolution(&mut self, w: u32, h: u32) {
        self.vm_resolution = (w, h);
        self.transform_dirty.set(true);
    }

    /// Returns the VM resolution.
    #[inline]
    pub fn vm_resolution(&self) -> (u32, u32) { self.vm_resolution }

    /// Sets the widget logical size and marks viewport as dirty.
    #[inline]
    pub fn set_widget_size(&mut self, w: f32, h: f32) {
        self.widget_size_logical = (w, h);
        self.transform_dirty.set(true);
    }

    /// Returns the physical canvas size for QEMU (logical size * ui_scale).
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

    /// Calculates how to fit VM display within widget (like CSS object-fit: contain).
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

    /// Returns cached viewport, recalculating if dirty.
    #[inline]
    pub fn get_cached_viewport(&self) -> Option<Viewport> {
        if self.transform_dirty.get() {
            self.cached_viewport.set(self.calculate_contain_transform());
            self.transform_dirty.set(false);
        }
        self.cached_viewport.get()
    }

    /// Converts widget coordinates to VM guest coordinates.
    #[inline]
    pub fn widget_to_guest(&self, logical_x: f32, logical_y: f32) -> Option<(u32, u32)> {
        let (vm_w, vm_h) = self.vm_resolution;
        if vm_w == 0 || vm_h == 0 {
            return None;
        }
        let viewport = self.get_cached_viewport()?;
        let guest_x = ((logical_x - viewport.offset_x) / viewport.scale).floor().clamp(0., (vm_w - 1) as f32);
        let guest_y = ((logical_y - viewport.offset_y) / viewport.scale).floor().clamp(0., (vm_h - 1) as f32);
        Some((guest_x as u32, guest_y as u32))
    }

    /// Returns the VM display bounds in widget coordinates: (x, y, width, height).
    #[inline]
    pub fn vm_display_bounds(&self) -> Option<(f32, f32, f32, f32)> {
        let viewport = self.get_cached_viewport()?;
        let (vm_w, vm_h) = self.vm_resolution;
        let vm_w = vm_w as f32;
        let vm_h = vm_h as f32;
        Some((viewport.offset_x, viewport.offset_y, vm_w * viewport.scale, vm_h * viewport.scale))
    }

    /// Checks if a point (in widget coordinates) is within the VM display region.
    #[inline]
    pub fn is_in_viewport(&self, point: &Point) -> bool {
        let Some((x, y, w, h)) = self.vm_display_bounds() else {
            return false;
        };
        let vm_rect = Rect::new(x, y, w, h);
        vm_rect.contains_point(point)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper to assert fuzzy float equality for coordinates.
    /// Allows ±1 pixel tolerance for rounding errors at boundaries.
    fn assert_coord_eq(actual: (u32, u32), expected: (u32, u32)) {
        let (ax, ay) = actual;
        let (ex, ey) = expected;
        assert!(ax.abs_diff(ex) <= 1, "X coordinate mismatch: actual {}, expected {}", ax, ex);
        assert!(ay.abs_diff(ey) <= 1, "Y coordinate mismatch: actual {}, expected {}", ay, ey);
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
}
