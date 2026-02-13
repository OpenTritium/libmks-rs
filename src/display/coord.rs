use relm4::gtk::graphene::Rect;

#[derive(Debug, Clone, Copy)]
pub struct Transform {
    pub scale: f32,
    pub offset_x: f32,
    pub offset_y: f32,
}

#[derive(Debug, Clone)]
pub struct CoordinateSystem {
    vm_resolution: (u32, u32),
    widget_size_logical: (f32, f32),
    surface_bounds: Option<Rect>,
    scale_factor: f32,
    mm_per_pixel: f64,
    cached_transform: Option<Transform>,
    transform_dirty: bool,
}

impl CoordinateSystem {
    pub fn new(vm_w: u32, vm_h: u32, widget_w: f32, widget_h: f32) -> Self {
        Self {
            vm_resolution: (vm_w, vm_h),
            widget_size_logical: (widget_w, widget_h),
            surface_bounds: None,
            scale_factor: 1.0,
            mm_per_pixel: 0.264583, // Default 96 DPI
            cached_transform: None,
            transform_dirty: true,
        }
    }

    #[inline]
    pub fn set_vm_resolution(&mut self, w: u32, h: u32) {
        self.vm_resolution = (w, h);
        self.transform_dirty = true;
    }

    #[inline]
    pub fn set_widget_size(&mut self, w: f32, h: f32) {
        self.widget_size_logical = (w, h);
        self.transform_dirty = true;
    }

    #[inline]
    pub fn set_surface_bounds(&mut self, bounds: Rect) { self.surface_bounds = Some(bounds); }

    #[inline]
    pub fn set_scale_factor(&mut self, factor: f32) {
        if self.scale_factor != factor {
            self.scale_factor = factor;
            self.transform_dirty = true;
        }
    }

    #[inline]
    pub fn set_mm_per_pixel(&mut self, mm_per_pixel: f64) {
        if (self.mm_per_pixel - mm_per_pixel).abs() > 0.0001 {
            self.mm_per_pixel = mm_per_pixel;
            self.transform_dirty = true;
        }
    }

    #[inline]
    pub const fn calculate_contain_transform(&self) -> Option<(f32, f32, f32)> {
        let (vm_w, vm_h) = self.vm_resolution;
        let (widget_w_logical, widget_h_logical) = self.widget_size_logical;
        if vm_w == 0 || vm_h == 0 || widget_w_logical <= 0.0 || widget_h_logical <= 0.0 {
            return None;
        }
        let vm_w = vm_w as f32;
        let vm_h = vm_h as f32;

        let widget_w = widget_w_logical * self.scale_factor;
        let widget_h = widget_h_logical * self.scale_factor;

        if widget_w * vm_h < widget_h * vm_w {
            let scale = widget_w / vm_w;
            let offset_x = 0.;
            let offset_y = (widget_h - vm_h * scale) / 2.;
            Some((scale, offset_x, offset_y))
        } else {
            let scale = widget_h / vm_h;
            let offset_x = (widget_w - vm_w * scale) / 2.;
            let offset_y = 0.;
            Some((scale, offset_x, offset_y))
        }
    }

    #[inline]
    pub fn get_cached_transform(&mut self) -> Option<Transform> {
        if self.transform_dirty {
            let (scale, offset_x, offset_y) = self.calculate_contain_transform()?;
            self.cached_transform = Some(Transform { scale, offset_x, offset_y });
            self.transform_dirty = false;
        }
        self.cached_transform
    }

    #[inline]
    pub fn get_scale_factor(&self) -> f32 { self.scale_factor }

    #[inline]
    pub fn get_mm_per_pixel(&self) -> f64 { self.mm_per_pixel }

    #[inline]
    pub fn widget_to_guest(&self, logical_x: f32, logical_y: f32) -> Option<(u32, u32)> {
        let (vm_w, vm_h) = self.vm_resolution;
        if vm_w == 0 || vm_h == 0 {
            return None;
        }
        let (scale, offset_x, offset_y) = self.calculate_contain_transform()?;

        let phys_x = logical_x * self.scale_factor;
        let phys_y = logical_y * self.scale_factor;

        let gx = ((phys_x - offset_x) / scale).clamp(0., (vm_w - 1) as f32) as u32;
        let gy = ((phys_y - offset_y) / scale).clamp(0., (vm_h - 1) as f32) as u32;

        Some((gx, gy))
    }

    #[inline]
    pub fn guest_to_surface(&self, x: u32, y: u32) -> Option<(f32, f32)> {
        let (vm_w, vm_h) = self.vm_resolution;
        if vm_w == 0 || vm_h == 0 {
            return None;
        }
        let bounds = self.surface_bounds?;

        let sx = bounds.x() + (x as f32 * bounds.width()) / (vm_w as f32);
        let sy = bounds.y() + (y as f32 * bounds.height()) / (vm_h as f32);
        Some((sx, sy))
    }

    #[inline]
    pub fn surface_to_guest(&self, x: f32, y: f32) -> Option<(u32, u32)> {
        let (vm_w, vm_h) = self.vm_resolution;
        let bounds = self.surface_bounds?;
        let bw = bounds.width();
        let bh = bounds.height();
        if vm_w == 0 || vm_h == 0 || bw <= 0. || bh <= 0. {
            return None;
        }

        let rel_x = (x - bounds.x()) / bw;
        let rel_y = (y - bounds.y()) / bh;

        let gx = (rel_x * vm_w as f32).clamp(0., (vm_w - 1) as f32) as u32;
        let gy = (rel_y * vm_h as f32).clamp(0., (vm_h - 1) as f32) as u32;
        Some((gx, gy))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_widget_to_guest_with_scale_factor() {
        let mut coord = CoordinateSystem::new(1920, 1080, 960.0, 540.0);
        coord.set_scale_factor(2.0);

        let result = coord.widget_to_guest(480.0, 270.0);
        assert!(result.is_some());
        let (gx, gy) = result.unwrap();
        assert!(gx > 0 && gx < 1920);
        assert!(gy > 0 && gy < 1080);
    }

    #[test]
    fn test_widget_to_guest_clamping() {
        let coord = CoordinateSystem::new(100, 100, 50.0, 50.0);

        let result = coord.widget_to_guest(1000.0, 1000.0);
        assert!(result.is_some());
        let (gx, gy) = result.unwrap();
        assert!(gx < 100);
        assert!(gy < 100);
    }

    #[test]
    fn test_surface_to_guest_mapping() {
        let mut coord = CoordinateSystem::new(1920, 1080, 960.0, 540.0);
        coord.set_scale_factor(2.0);

        use relm4::gtk::graphene::Rect;
        let bounds = Rect::new(0.0, 0.0, 960.0, 540.0);
        coord.set_surface_bounds(bounds);

        let result = coord.surface_to_guest(480.0, 270.0);
        assert!(result.is_some());
        let (gx, gy) = result.unwrap();
        assert!(gx > 0 && gx < 1920);
        assert!(gy > 0 && gy < 1080);
    }

    #[test]
    fn test_guest_to_surface_mapping() {
        let mut coord = CoordinateSystem::new(1920, 1080, 960.0, 540.0);
        coord.set_scale_factor(2.0);

        use relm4::gtk::graphene::Rect;
        let bounds = Rect::new(0.0, 0.0, 960.0, 540.0);
        coord.set_surface_bounds(bounds);

        let result = coord.guest_to_surface(960, 540);
        assert!(result.is_some());
        let (sx, sy) = result.unwrap();
        assert!(sx >= 0.0 && sx <= 960.0);
        assert!(sy >= 0.0 && sy <= 540.0);
    }
}
