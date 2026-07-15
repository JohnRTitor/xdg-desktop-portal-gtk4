use {
    crate::{gui::UiProxy, utils::external_window::set_wayland_parent},
    async_channel::{Receiver, Sender},
    gtk4::{
        DialogFlags, Entry, Image, MessageDialog, MessageType, ResponseType, Widget,
        glib::{self, MainContext},
        prelude::{BoxExt, Cast, DialogExt, EditableExt, GtkWindowExt, WidgetExt},
    },
    rust_i18n::t,
    std::path::Path,
    thiserror::Error,
};

#[derive(Debug, Error)]
pub enum AccountError {
    #[error("Operation could not be started")]
    Closed,
    #[error("Operation was rejected")]
    Rejected,
}

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
    pub async fn run(self, proxy: &UiProxy) -> Result<AccountResult, AccountError> {
        let (send, recv) = async_channel::bounded(1);
        let (_send, close_on_close) = async_channel::bounded(1);
        let context = proxy.context.clone();
        proxy
            .context
            .invoke(move || self.run_impl(send, context, close_on_close));
        recv.recv().await.map_err(|_| AccountError::Closed)?
    }

    fn run_impl(
        self,
        send: Sender<Result<AccountResult, AccountError>>,
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

        let dialog = MessageDialog::new(
            None::<&gtk4::Window>,
            DialogFlags::MODAL,
            MessageType::Question,
            gtk4::ButtonsType::None,
            &*title,
        );

        dialog.format_secondary_text(Some(&subtitle));

        dialog.add_button(&t!("deny_action"), ResponseType::Cancel);
        dialog.add_button(&t!("share_action"), ResponseType::Ok);

        if let Ok(area) = dialog.message_area().downcast::<gtk4::Box>() {
            let hbox = gtk4::Box::new(gtk4::Orientation::Horizontal, 12);
            hbox.set_margin_top(10);
            area.append(&hbox);

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

            dialog.connect_response(move |d, r| {
                let res = match r {
                    ResponseType::Ok => {
                        let image_uri = if !icon_file.is_empty() {
                            if let Ok(uri) = glib::filename_to_uri(&icon_file, None) {
                                uri.to_string()
                            } else {
                                String::new()
                            }
                        } else {
                            String::new()
                        };

                        Ok(AccountResult {
                            user_name: user_name_entry.text().to_string(),
                            real_name: real_name_entry.text().to_string(),
                            image: image_uri,
                        })
                    }
                    _ => Err(AccountError::Rejected),
                };
                let _ = send.send_blocking(res);
                d.close();
            });
        } else {
            log::error!("Failed to downcast message_area to Box in AccountDialog");
            let _ = send.send_blocking(Err(AccountError::Rejected));
        }

        dialog.upcast_ref::<Widget>().realize();
        set_wayland_parent(dialog.upcast_ref::<Widget>(), &self.parent_window);

        dialog.show();
        context.spawn_local(async move {
            let _ = close_on_close.recv().await;
            dialog.close();
        });
    }
}
