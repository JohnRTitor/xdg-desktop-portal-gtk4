use {
    crate::{gui::UiProxy, utils::external_window::set_wayland_parent},
    async_channel::{Receiver, Sender},
    gtk4::{
        glib::MainContext,
        prelude::{Cast, DialogExt, GtkWindowExt, WidgetExt},
        PrintUnixDialog, ResponseType, Widget,
    },
    rust_i18n::t,
    std::collections::HashMap,
    thiserror::Error,
    zbus::zvariant::OwnedValue,
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

pub struct PrintResult {
    pub settings: HashMap<String, OwnedValue>,
    pub page_setup: HashMap<String, OwnedValue>,
}

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
        let dialog = PrintUnixDialog::new(Some(&self.title), None::<&gtk4::Window>);
        dialog.set_modal(true);
        
        dialog.upcast_ref::<Widget>().realize();
        set_wayland_parent(dialog.upcast_ref::<Widget>(), &self.parent_window);

        dialog.connect_response(move |d, r| {
            let res = match r {
                ResponseType::Ok => {
                    let mut settings_map = HashMap::new();
                    let mut page_setup_map = HashMap::new();

                    let settings = d.settings();
                    settings.foreach(|k, v| {
                        if let Ok(owned) = zbus::zvariant::OwnedValue::try_from(zbus::zvariant::Value::from(v)) {
                            settings_map.insert(k.to_string(), owned);
                        }
                    });

                    let page_setup = d.page_setup();
                    let key_file = gtk4::glib::KeyFile::new();
                    page_setup.to_key_file(&key_file, Some("Page Setup"));
                    if let Ok(keys) = key_file.keys("Page Setup") {
                        for key in keys {
                            if let Ok(val) = key_file.value("Page Setup", &key) {
                                if let Ok(owned) = zbus::zvariant::OwnedValue::try_from(zbus::zvariant::Value::from(val.as_str())) {
                                    page_setup_map.insert(key.to_string(), owned);
                                }
                            }
                        }
                    }

                    Ok(PrintResult {
                        settings: settings_map,
                        page_setup: page_setup_map,
                    })
                },
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
