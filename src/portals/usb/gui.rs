use crate::{gui::UiProxy, utils::external_window::set_wayland_parent};
use async_channel::{Receiver, Sender};
use gtk4::{
    Box as GtkBox, CheckButton, Label, ListBox, ListBoxRow, Orientation, ResponseType,
    ScrolledWindow, Widget, glib::MainContext, prelude::*,
};
use rust_i18n::t;
use std::collections::HashMap;
use std::rc::Rc;
use thiserror::Error;
use zbus::zvariant::OwnedValue;

#[derive(Debug, Error)]
pub enum UsbError {
    #[error("Operation could not be started")]
    Closed,
    #[error("Operation was rejected")]
    Rejected,
}

#[derive(Debug, Clone)]
pub struct UsbDevice {
    pub id: String,
    pub title: String,
    pub subtitle: String,
    pub serial: Option<String>,
    pub access_options: HashMap<String, OwnedValue>,
}

pub struct UsbUi {
    pub app_id: String,
    pub parent_window: String,
    pub devices: Vec<UsbDevice>,
}

pub struct UsbResult {
    pub devices: Vec<(String, HashMap<String, OwnedValue>)>,
}

impl UsbUi {
    pub async fn run(self, proxy: &UiProxy) -> Result<UsbResult, UsbError> {
        let (send, recv) = async_channel::bounded(1);
        let (_send, close_on_close) = async_channel::bounded(1);
        let context = proxy.context.clone();
        proxy
            .context
            .invoke(move || self.run_impl(send, context, close_on_close));
        recv.recv().await.map_err(|_| UsbError::Closed)?
    }

    fn run_impl(
        self,
        send: Sender<Result<UsbResult, UsbError>>,
        context: MainContext,
        close_on_close: Receiver<()>,
    ) {
        let dialog = gtk4::Dialog::builder()
            .title(t!("allow_usb_access").to_string())
            .modal(true)
            .default_width(400)
            .default_height(400)
            .build();

        dialog.add_button(&t!("cancel_action"), ResponseType::Cancel);
        let ok_button = dialog.add_button(&t!("allow_action"), ResponseType::Ok);
        ok_button.set_sensitive(false);

        let content_area = dialog.content_area();
        content_area.set_margin_top(12);
        content_area.set_margin_bottom(12);
        content_area.set_margin_start(12);
        content_area.set_margin_end(12);
        content_area.set_spacing(12);

        let label_text = format!(
            "{} {}",
            self.app_id,
            t!("wants_to_access_usb_devices")
        );
        let label = Label::builder().label(&label_text).wrap(true).build();
        content_area.append(&label);

        let scrolled_window = ScrolledWindow::builder()
            .hscrollbar_policy(gtk4::PolicyType::Never)
            .vscrollbar_policy(gtk4::PolicyType::Automatic)
            .vexpand(true)
            .build();
        content_area.append(&scrolled_window);

        let list_box = ListBox::new();
        list_box.set_selection_mode(gtk4::SelectionMode::None);
        scrolled_window.set_child(Some(&list_box));

        let checks: Rc<Vec<CheckButton>> =
            Rc::new(self.devices.iter().map(|_| CheckButton::new()).collect());

        for (i, device) in self.devices.iter().enumerate() {
            let row = ListBoxRow::new();
            let hbox = GtkBox::new(Orientation::Horizontal, 12);
            hbox.set_margin_top(6);
            hbox.set_margin_bottom(6);
            hbox.set_margin_start(6);
            hbox.set_margin_end(6);

            let check = &checks[i];
            hbox.append(check);

            let vbox = GtkBox::new(Orientation::Vertical, 4);
            let title_label = Label::builder()
                .label(&device.title)
                .halign(gtk4::Align::Start)
                .build();
            let subtitle_label = Label::builder()
                .label(&device.subtitle)
                .halign(gtk4::Align::Start)
                .build();
            subtitle_label.add_css_class("dim-label");

            vbox.append(&title_label);
            vbox.append(&subtitle_label);

            if let Some(serial) = &device.serial {
                let serial_label = Label::builder()
                    .label(&format!("SN: {}", serial))
                    .halign(gtk4::Align::Start)
                    .build();
                serial_label.add_css_class("dim-label");
                vbox.append(&serial_label);
            }

            hbox.append(&vbox);
            row.set_child(Some(&hbox));
            list_box.append(&row);

            let ok_button_clone = ok_button.clone();
            let checks_clone = checks.clone();
            check.connect_toggled(move |_| {
                let any_checked = checks_clone.iter().any(|c| c.is_active());
                ok_button_clone.set_sensitive(any_checked);
            });
        }

        let checks_final = checks.clone();
        let devices = self.devices;
        dialog.connect_response(move |d, r| {
            let res = match r {
                ResponseType::Ok => {
                    let mut selected = Vec::new();
                    for (i, check) in checks_final.iter().enumerate() {
                        if check.is_active() {
                            selected
                                .push((devices[i].id.clone(), devices[i].access_options.clone()));
                        }
                    }
                    if selected.is_empty() {
                        Err(UsbError::Rejected)
                    } else {
                        Ok(UsbResult { devices: selected })
                    }
                }
                _ => Err(UsbError::Rejected),
            };
            let _ = send.send_blocking(res);
            d.close();
        });

        if let Some(w) = dialog.upcast_ref::<Widget>().downcast_ref::<gtk4::Window>() {
            set_wayland_parent(w.upcast_ref::<Widget>(), &self.parent_window);
        }

        dialog.show();
        context.spawn_local(async move {
            let _ = close_on_close.recv().await;
            dialog.close();
        });
    }
}
