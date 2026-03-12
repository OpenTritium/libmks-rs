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

impl CropInfo {
    #[inline]
    pub const fn from_width_height(w: f32, h: f32) -> Self { Self { x: 0., y: 0., width: w, height: h } }
}
