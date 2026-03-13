use super::vm_display::{Message, VmDisplayModel};
use crate::{mks_debug, mks_error, mks_warn};
use relm4::{
    ComponentSender,
    gtk::{DrawingArea, prelude::*},
};
use std::hint::unlikely;

const LOG_TARGET: &str = "mks.display.monitor";

fn update_monitor_info(widget: &DrawingArea, sender: &ComponentSender<VmDisplayModel>) {
    let display = widget.display();
    let Some(native) = widget.native() else {
        mks_error!("Input overlay has no native widget; skipping monitor metrics update");
        return;
    };
    let Some(surface) = native.surface() else {
        mks_error!("Native widget has no surface; skipping monitor metrics update");
        return;
    };
    let Some(monitor) = display.monitor_at_surface(&surface) else {
        mks_error!("No monitor found for surface; skipping monitor metrics update");
        return;
    };
    let geometry = monitor.geometry();
    let w_mm = monitor.width_mm() as f32;
    let h_mm = monitor.height_mm() as f32;
    let scale_factor = (widget.scale_factor() as f32).max(0.5).clamp(0.5, 8.);
    let geometry_width_physical = geometry.width() as f32 * scale_factor;
    let geometry_height_physical = geometry.height() as f32 * scale_factor;
    let invalid_cond = w_mm <= 0.
        || h_mm <= 0.
        || geometry_width_physical <= 0.
        || geometry_height_physical <= 0.
        || !w_mm.is_finite()
        || !h_mm.is_finite()
        || !geometry_width_physical.is_finite()
        || !geometry_height_physical.is_finite();
    if unlikely(invalid_cond) {
        mks_warn!(
            "Invalid monitor dimensions: {w_mm}×{h_mm}mm physical, {}×{} logical px (scale={scale_factor:.2}); \
             skipping pixel pitch calculation",
            geometry.width(),
            geometry.height(),
        );
        return;
    }
    let ppm = w_mm / geometry_width_physical;
    mks_debug!(
        "Monitor {}: {w_mm}×{h_mm}mm, {}×{} logical px (scale={scale_factor:.2}) → \
         {geometry_width_physical}×{geometry_height_physical} physical px (pitch={ppm:.4}mm/px)",
        monitor.model().as_deref().unwrap_or("unknown"),
        geometry.width(),
        geometry.height(),
    );
    sender.input(Message::UpdateMonitorInfo { pixel_pitch_mm: ppm });
}

pub fn attach_resize_handlers(input_overlay: &DrawingArea, sender: &ComponentSender<VmDisplayModel>) {
    let sender_realize = sender.clone();
    input_overlay.connect_realize(move |widget| {
        update_monitor_info(widget, &sender_realize);
    });

    let sender_resize = sender.clone();
    input_overlay.connect_resize(move |widget, _, _| {
        update_monitor_info(widget, &sender_resize);
        let w = widget.width() as f32;
        let h = widget.height() as f32;
        let invalid_cond = w <= 0. || h <= 0. || !w.is_finite() || !h.is_finite();
        if unlikely(invalid_cond) {
            mks_error!("Ignoring canvas resize with invalid dimensions: ({w}, {h})");
            return;
        }
        sender_resize.input(Message::CanvasResize { logical_width: w, logical_height: h });
    });

    let sender_scale = sender.clone();
    input_overlay.connect_scale_factor_notify(move |widget| {
        update_monitor_info(widget, &sender_scale);
    });
}
