use {
    crate::gui::{UiError, UiProxy},
    async_channel::{Receiver, Sender},
    gtk4::{
        Button, Entry, Image, Label,
        glib::MainContext,
        prelude::{BoxExt, ButtonExt, EditableExt, GtkWindowExt, WidgetExt},
    },
    rust_i18n::t,
};

pub struct DynamicLauncherUi {
    pub app_id: String,
    pub parent_window: String,
    pub name: String,
    pub editable_name: bool,
    pub icon_name: Option<String>,
    pub icon_data: Option<Vec<u8>>,
}

pub struct DynamicLauncherResult {
    pub name: String,
}

impl DynamicLauncherUi {
    pub async fn run(self, proxy: &UiProxy) -> Result<DynamicLauncherResult, UiError> {
        crate::gui::run_ui_task(
            proxy,
            |send, context, close_on_close| self.run_impl(send, context, close_on_close),
            || UiError::Closed,
        )
        .await
    }

    fn run_impl(
        self,
        send: Sender<Result<DynamicLauncherResult, UiError>>,
        context: MainContext,
        close_on_close: Receiver<()>,
    ) {
        let title = t!("create_web_application");
        let subtitle = format!("{} wants to create a web application.", self.app_id);

        let dialog = crate::gui::dialog::CustomDialog::new(&title, true);

        let cancel_button = Button::with_label(&t!("cancel_action"));
        let ok_button = Button::with_label(&t!("create_action"));
        ok_button.add_css_class("suggested-action");

        dialog.action_area.append(&cancel_button);
        dialog.action_area.append(&ok_button);

        let subtitle_lbl = Label::new(Some(&subtitle));
        subtitle_lbl.add_css_class("title-2");
        subtitle_lbl.set_halign(gtk4::Align::Start);
        dialog.content_area.append(&subtitle_lbl);

        let hbox = gtk4::Box::new(gtk4::Orientation::Horizontal, 12);
        hbox.set_margin_top(10);
        dialog.content_area.append(&hbox);

        if let Some(bytes) = &self.icon_data {
            // Convert raw bytes to a GTK `BytesIcon`
            let bytes_glib = gtk4::glib::Bytes::from(bytes);
            let icon = gtk4::gio::BytesIcon::new(&bytes_glib);
            let image = Image::from_gicon(&icon);
            image.set_pixel_size(64);
            hbox.append(&image);
        } else if let Some(icon) = &self.icon_name {
            let image = Image::from_icon_name(icon);
            image.set_pixel_size(64);
            hbox.append(&image);
        } else {
            // Fallback icon if none provided
            let image = Image::from_icon_name("application-x-executable");
            image.set_pixel_size(64);
            hbox.append(&image);
        }

        let vbox = gtk4::Box::new(gtk4::Orientation::Vertical, 6);
        hbox.append(&vbox);

        let name_label = Label::new(Some(&t!("name")));
        name_label.set_halign(gtk4::Align::Start);
        vbox.append(&name_label);

        let name_entry = Entry::new();
        name_entry.set_text(&self.name);
        name_entry.set_editable(self.editable_name);
        vbox.append(&name_entry);

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
            let res = Ok(DynamicLauncherResult {
                name: name_entry.text().to_string(),
            });
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
