use {
    crate::gui::{UiError, UiProxy},
    async_channel::{Receiver, Sender},
    gtk4::{
        Box as GtkBox, Button, CheckButton, Label, ListBox, ListBoxRow, Orientation,
        ScrolledWindow, glib::MainContext, prelude::*,
    },
    rust_i18n::t,
    std::{collections::HashMap, rc::Rc},
    zbus::zvariant::OwnedValue,
};

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
    pub async fn run(self, proxy: &UiProxy) -> Result<UsbResult, UiError> {
        crate::gui::run_ui_task(
            proxy,
            |send, context, close_on_close| self.run_impl(send, context, close_on_close),
            || UiError::Closed,
        )
        .await
    }

    fn run_impl(
        self,
        send: Sender<Result<UsbResult, UiError>>,
        context: MainContext,
        close_on_close: Receiver<()>,
    ) {
        let dialog = crate::gui::dialog::CustomDialog::new(&t!("allow_usb_access"), true);

        let cancel_button = Button::with_label(&t!("cancel_action"));
        let ok_button = Button::with_label(&t!("allow_action"));
        ok_button.set_sensitive(false);
        ok_button.add_css_class("suggested-action");

        dialog.action_area.append(&cancel_button);
        dialog.action_area.append(&ok_button);

        let label_text = format!("{} {}", self.app_id, t!("wants_to_access_usb_devices"));
        let label = Label::builder().label(&label_text).wrap(true).build();
        dialog.content_area.append(&label);

        let scrolled_window = ScrolledWindow::builder()
            .hscrollbar_policy(gtk4::PolicyType::Never)
            .vscrollbar_policy(gtk4::PolicyType::Automatic)
            .vexpand(true)
            .build();
        dialog.content_area.append(&scrolled_window);

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
        let window = dialog.window.clone();

        let send_close = send.clone();
        window.connect_close_request(move |_| {
            let _ = send_close.send_blocking(Err(UiError::Rejected));
            gtk4::glib::Propagation::Proceed
        });

        let send_cancel = send.clone();
        let w_cancel = window.clone();
        cancel_button.connect_clicked(move |_| {
            let _ = send_cancel.send_blocking(Err(UiError::Rejected));
            w_cancel.close();
        });

        let send_ok = send.clone();
        let w_ok = window.clone();
        ok_button.connect_clicked(move |_| {
            let mut selected = Vec::new();
            for (i, check) in checks_final.iter().enumerate() {
                if check.is_active() {
                    selected.push((devices[i].id.clone(), devices[i].access_options.clone()));
                }
            }
            let res = if selected.is_empty() {
                Err(UiError::Rejected)
            } else {
                Ok(UsbResult { devices: selected })
            };
            let _ = send_ok.send_blocking(res);
            w_ok.close();
        });

        crate::gui::setup_wayland(&window, &self.parent_window);

        window.show();
        context.spawn_local(async move {
            let _ = close_on_close.recv().await;
            window.close();
        });
    }
}
