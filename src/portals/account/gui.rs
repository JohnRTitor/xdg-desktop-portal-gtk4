use {
    crate::gui::{UiError, UiProxy},
    async_channel::{Receiver, Sender},
    gtk4::{
        Button, Entry, Image, Label,
        glib::{self, MainContext},
        prelude::{BoxExt, ButtonExt, EditableExt, GtkWindowExt, WidgetExt},
    },
    rust_i18n::t,
    std::path::Path,
};

pub struct AccountUi {
    pub app_id: String,
    pub parent_window: String,
    pub user_name: String,
    pub real_name: String,
    pub icon_file: String,
    pub reason: Option<String>,
}

pub struct AccountResult {
    pub user_name: String,
    pub real_name: String,
    pub image: String,
}

impl AccountUi {
    pub async fn run(self, proxy: &UiProxy) -> Result<AccountResult, UiError> {
        crate::gui::run_ui_task(
            proxy,
            |send, context, close_on_close| self.run_impl(send, context, close_on_close),
            || UiError::Closed,
        )
        .await
    }

    fn run_impl(
        self,
        send: Sender<Result<AccountResult, UiError>>,
        context: MainContext,
        close_on_close: Receiver<()>,
    ) {
        let title = t!("share_information");
        let subtitle = if let Some(reason) = &self.reason {
            format!(
                "{} ({})",
                t!("application_wants_to_access_information"),
                reason
            )
        } else {
            t!("application_wants_to_access_information").to_string()
        };

        let dialog = crate::gui::dialog::CustomDialog::new(&title, true);

        let cancel_button = Button::with_label(&t!("deny_action"));
        let ok_button = Button::with_label(&t!("share_action"));
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

        let image = if !self.icon_file.is_empty() && Path::new(&self.icon_file).exists() {
            Image::from_file(&self.icon_file)
        } else {
            Image::from_icon_name("avatar-default")
        };
        image.set_pixel_size(64);
        hbox.append(&image);

        let vbox = gtk4::Box::new(gtk4::Orientation::Vertical, 6);
        hbox.append(&vbox);

        let real_name_entry = Entry::new();
        real_name_entry.set_text(&self.real_name);
        vbox.append(&real_name_entry);

        let user_name_entry = Entry::new();
        user_name_entry.set_text(&self.user_name);
        vbox.append(&user_name_entry);

        let icon_file = self.icon_file.clone();

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
            let image_uri = if !icon_file.is_empty() {
                if let Ok(uri) = glib::filename_to_uri(&icon_file, None) {
                    uri.to_string()
                } else {
                    String::new()
                }
            } else {
                String::new()
            };

            let res = Ok(AccountResult {
                user_name: user_name_entry.text().to_string(),
                real_name: real_name_entry.text().to_string(),
                image: image_uri,
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
