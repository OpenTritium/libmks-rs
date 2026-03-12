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
