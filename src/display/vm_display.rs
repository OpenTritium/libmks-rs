use crate::{
    dbus::listener::QemuEvent,
    display::screen::{Screen, UpdateFlags},
};
use kanal::AsyncReceiver;
use log::warn;
use relm4::{
    gtk::{ContentFit, Fixed, GraphicsOffload, GraphicsOffloadEnabled, Overlay, Picture, prelude::*},
    prelude::*,
};

#[derive(Debug)]
pub enum DisplayMsg {
    Qemu(QemuEvent),
    Resize,
}

pub struct VmDisplayModel {
    pub screen: Screen,
    pub changes: UpdateFlags,
}

pub struct VmDisplayWidgets {
    pub vm_picture: Picture,
    pub cursor_layer: Fixed,
    pub cursor_picture: Picture,
}

pub struct VmDisplayInit {
    pub rx: AsyncReceiver<QemuEvent>,
}

impl Component for VmDisplayModel {
    type CommandOutput = ();
    type Init = VmDisplayInit;
    type Input = DisplayMsg;
    type Output = ();
    type Root = Overlay;
    type Widgets = VmDisplayWidgets;

    fn init_root() -> Self::Root { Overlay::builder().hexpand(true).vexpand(true).build() }

    fn init(init: Self::Init, root: Self::Root, sender: ComponentSender<Self>) -> ComponentParts<Self> {
        let model = VmDisplayModel { screen: Screen::new(), changes: UpdateFlags::default() };
        let offload = GraphicsOffload::builder().enabled(GraphicsOffloadEnabled::Enabled).build();
        let vm_picture = Picture::builder().can_shrink(true).content_fit(ContentFit::Contain).build();

        let sender_clone = sender.clone();
        vm_picture.connect_notify_local(Some("allocation"), move |_, _| {
            sender_clone.input(DisplayMsg::Resize);
        });

        offload.set_child(Some(&vm_picture));
        root.add_overlay(&offload);
        let cursor_layer = Fixed::builder().can_target(false).hexpand(true).vexpand(true).build();
        let cursor_picture = Picture::builder().can_shrink(true).content_fit(ContentFit::Fill).build();
        cursor_layer.put(&cursor_picture, 0.0, 0.0);
        root.add_overlay(&cursor_layer);
        let widgets = VmDisplayWidgets { vm_picture, cursor_layer, cursor_picture };
        relm4::spawn(async move {
            while let Ok(event) = init.rx.recv().await {
                sender.input(DisplayMsg::Qemu(event));
            }
            warn!("VM display channel closed, close the vm display");
            sender.input(DisplayMsg::Qemu(QemuEvent::Disable));
        });
        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, _sender: ComponentSender<Self>, _root: &Self::Root) {
        match msg {
            DisplayMsg::Qemu(event) => match self.screen.handle_event(event) {
                Ok(flags) => self.changes = flags,
                Err(_) => {
                    self.changes = UpdateFlags::default();
                }
            },
            DisplayMsg::Resize => {
                self.changes.cursor = true;
            }
        }
    }

    fn update_view(&self, widgets: &mut Self::Widgets, _sender: ComponentSender<Self>) {
        if self.changes.frame {
            if let Some(texture) = self.screen.get_background_texture() {
                widgets.vm_picture.set_paintable(Some(texture));
            } else {
                widgets.vm_picture.set_paintable(None::<&relm4::gtk::gdk::Texture>);
            }
        }
        if self.changes.cursor || self.changes.frame {
            let cursor = &self.screen.cursor;
            widgets.cursor_picture.set_visible(cursor.visible);
            if cursor.visible {
                let widget_w = widgets.vm_picture.width() as f64;
                let widget_h = widgets.vm_picture.height() as f64;
                let (vm_w, vm_h) = self.screen.resolution();
                if vm_w != 0 && vm_h != 0 {
                    let vm_w = vm_w as f64;
                    let vm_h = vm_h as f64;
                    let scale_x = widget_w / vm_w;
                    let scale_y = widget_h / vm_h;
                    let scale = scale_x.min(scale_y);
                    let drawn_w = vm_w * scale;
                    let drawn_h = vm_h * scale;
                    let offset_x = (widget_w - drawn_w) / 2.;
                    let offset_y = (widget_h - drawn_h) / 2.;
                    let final_x = offset_x + ((cursor.x - cursor.hot_x) as f64 * scale);
                    let final_y = offset_y + ((cursor.y - cursor.hot_y) as f64 * scale);
                    widgets.cursor_layer.move_(&widgets.cursor_picture, final_x, final_y);
                    if let Some(tex) = &cursor.texture {
                        widgets.cursor_picture.set_paintable(Some(tex));
                        let cursor_w = (tex.width() as f64 * scale).ceil() as i32;
                        widgets.cursor_picture.set_width_request(cursor_w);
                        let cursor_h = (tex.height() as f64 * scale).ceil() as i32;
                        widgets.cursor_picture.set_height_request(cursor_h);
                    }
                } else {
                    let x = (cursor.x - cursor.hot_x) as f64;
                    let y = (cursor.y - cursor.hot_y) as f64;
                    widgets.cursor_layer.move_(&widgets.cursor_picture, x, y);
                    if let Some(tex) = &cursor.texture {
                        widgets.cursor_picture.set_paintable(Some(tex));
                    }
                }
            }
        }
    }
}
