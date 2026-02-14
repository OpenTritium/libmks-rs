use std::cell::Cell;

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
    cached_transform: Cell<Option<Transform>>,
    transform_dirty: Cell<bool>,
}

impl CoordinateSystem {
    pub fn new(vm_w: u32, vm_h: u32, widget_w: f32, widget_h: f32) -> Self {
        Self {
            vm_resolution: (vm_w, vm_h),
            widget_size_logical: (widget_w, widget_h),
            cached_transform: Cell::new(None),
            transform_dirty: Cell::new(true),
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

    #[inline]
    pub const fn calculate_contain_transform(&self) -> Option<(f32, f32, f32)> {
        let (vm_w, vm_h) = self.vm_resolution;
        let (widget_w, widget_h) = self.widget_size_logical;
        if vm_w == 0 || vm_h == 0 || widget_w <= 0.0 || widget_h <= 0.0 {
            return None;
        }
        let vm_w = vm_w as f32;
        let vm_h = vm_h as f32;
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
    pub fn get_cached_transform(&self) -> Option<Transform> {
        if self.transform_dirty.get() {
            let new_transform = self.calculate_contain_transform().map(|(scale, offset_x, offset_y)| Transform {
                scale,
                offset_x,
                offset_y,
            });
            self.cached_transform.set(new_transform);
            self.transform_dirty.set(false);
        }
        self.cached_transform.get()
    }

    #[inline]
    pub fn widget_to_guest(&self, logical_x: f32, logical_y: f32) -> Option<(u32, u32)> {
        let (vm_w, vm_h) = self.vm_resolution;
        if vm_w == 0 || vm_h == 0 {
            return None;
        }
        let transform = self.get_cached_transform()?;
        let gx = ((logical_x - transform.offset_x) / transform.scale).clamp(0., (vm_w - 1) as f32) as u32;
        let gy = ((logical_y - transform.offset_y) / transform.scale).clamp(0., (vm_h - 1) as f32) as u32;
        Some((gx, gy))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_widget_to_guest_pure_logical_mapping() {
        let coord = CoordinateSystem::new(1920, 1080, 960.0, 540.0);

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
}
