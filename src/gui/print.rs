use {
    crate::{gui::UiProxy, utils::external_window::set_wayland_parent},
    async_channel::{Receiver, Sender},
    gtk4::{
        glib::MainContext,
        prelude::{Cast, DialogExt, GtkWindowExt, WidgetExt},
        DialogFlags, MessageDialog, MessageType, ResponseType, Widget,
    },
    rust_i18n::t,
    thiserror::Error,
};

#[derive(Debug, Error)]
pub enum PrintError {
    #[error("Operation could not be started")]
    Closed,
    #[error("Operation was rejected")]
    Rejected,
}

pub struct PrintUi {
    pub app_id: String,
    pub parent_window: String,
    pub title: String,
}

pub struct PrintResult {}

impl PrintUi {
    pub async fn run(self, proxy: &UiProxy) -> Result<PrintResult, PrintError> {
        let (send, recv) = async_channel::bounded(1);
        let (_send, close_on_close) = async_channel::bounded(1);
        let context = proxy.context.clone();
        proxy
            .context
            .invoke(move || self.run_impl(send, context, close_on_close));
        recv.recv().await.map_err(|_| PrintError::Closed)?
    }

    fn run_impl(
        self,
        send: Sender<Result<PrintResult, PrintError>>,
        context: MainContext,
        close_on_close: Receiver<()>,
    ) {
        // Due to complexity of PrintUnixDialog in gtk4-rs without explicit features, 
        // we'll show a basic message dialog for now indicating print preparation.
        // Full parity requires gtk4 print backend integration which is complex to proxy.

        let dialog = MessageDialog::new(
            None::<&gtk4::Window>,
            DialogFlags::MODAL,
            MessageType::Info,
            gtk4::ButtonsType::OkCancel,
            &*t!("Prepare Print"),
        );

        let subtitle = format!("{} wants to print: {}", self.app_id, self.title);
        dialog.format_secondary_text(Some(&subtitle));

        dialog.upcast_ref::<Widget>().realize();
        set_wayland_parent(dialog.upcast_ref::<Widget>(), &self.parent_window);

        dialog.connect_response(move |d, r| {
            let res = match r {
                ResponseType::Ok => Ok(PrintResult {}),
                _ => Err(PrintError::Rejected),
            };
            let _ = send.send_blocking(res);
            d.close();
        });

        dialog.show();
        context.spawn_local(async move {
            let _ = close_on_close.recv().await;
            dialog.close();
        });
    }
}
