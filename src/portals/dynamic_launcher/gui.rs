use {
    crate::{gui::UiProxy, utils::external_window::set_wayland_parent},
    async_channel::{Receiver, Sender},
    gtk4::{
        glib::MainContext,
        prelude::{BoxExt, Cast, DialogExt, EditableExt, GtkWindowExt, WidgetExt},
        DialogFlags, Entry, Image, Label, MessageDialog, MessageType, ResponseType, Widget,
    },
    rust_i18n::t,
    thiserror::Error,
};

#[derive(Debug, Error)]
pub enum DynamicLauncherError {
    #[error("Operation could not be started")]
    Closed,
    #[error("Operation was rejected")]
    Rejected,
}

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
    pub async fn run(self, proxy: &UiProxy) -> Result<DynamicLauncherResult, DynamicLauncherError> {
        let (send, recv) = async_channel::bounded(1);
        let (_send, close_on_close) = async_channel::bounded(1);
        let context = proxy.context.clone();
        proxy
            .context
            .invoke(move || self.run_impl(send, context, close_on_close));
        recv.recv().await.map_err(|_| DynamicLauncherError::Closed)?
    }

    fn run_impl(
        self,
        send: Sender<Result<DynamicLauncherResult, DynamicLauncherError>>,
        context: MainContext,
        close_on_close: Receiver<()>,
    ) {
        let title = t!("Create Web Application");
        let subtitle = format!("{} wants to create a web application.", self.app_id);

        let dialog = MessageDialog::new(
            None::<&gtk4::Window>,
            DialogFlags::MODAL,
            MessageType::Question,
            gtk4::ButtonsType::None,
            &*title,
        );

        dialog.format_secondary_text(Some(&subtitle));

        dialog.add_button(&t!("_Cancel"), ResponseType::Cancel);
        dialog.add_button(&t!("_Create"), ResponseType::Ok);

        if let Ok(area) = dialog.message_area().downcast::<gtk4::Box>() {
            let hbox = gtk4::Box::new(gtk4::Orientation::Horizontal, 12);
            hbox.set_margin_top(10);
            area.append(&hbox);

            if let Some(bytes) = &self.icon_data {
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
                let image = Image::from_icon_name("application-x-executable");
                image.set_pixel_size(64);
                hbox.append(&image);
            }

            let vbox = gtk4::Box::new(gtk4::Orientation::Vertical, 6);
            hbox.append(&vbox);

            let name_label = Label::new(Some(&t!("Name")));
            name_label.set_halign(gtk4::Align::Start);
            vbox.append(&name_label);

            let name_entry = Entry::new();
            name_entry.set_text(&self.name);
            name_entry.set_editable(self.editable_name);
            vbox.append(&name_entry);

            dialog.connect_response(move |d, r| {
                let res = match r {
                    ResponseType::Ok => Ok(DynamicLauncherResult {
                        name: name_entry.text().to_string(),
                    }),
                    _ => Err(DynamicLauncherError::Rejected),
                };
                let _ = send.send_blocking(res);
                d.close();
            });
        } else {
            log::error!("Failed to downcast message_area to Box in DynamicLauncherDialog");
            let _ = send.send_blocking(Err(DynamicLauncherError::Rejected));
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
