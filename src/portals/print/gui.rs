use {
    crate::gui::{UiError, UiProxy},
    async_channel::{Receiver, Sender},
    gtk4::{
        PrintUnixDialog, ResponseType,
        glib::MainContext,
        prelude::{DialogExt, GtkWindowExt, WidgetExt},
    },
    std::{cell::RefCell, collections::HashMap},
    zbus::zvariant::OwnedValue,
};

pub struct CachedPrintJob {
    pub app_id: String,
    pub title: String,
    pub printer: gtk4::Printer,
    pub settings: gtk4::PrintSettings,
    pub page_setup: gtk4::PageSetup,
}

thread_local! {
    pub static PRINT_JOBS: RefCell<HashMap<u32, CachedPrintJob>> = RefCell::new(HashMap::new());
}

pub struct PrintUi {
    pub app_id: String,
    pub parent_window: String,
    pub title: String,
}

pub struct PrintResult {
    pub token: u32,
    pub settings: HashMap<String, OwnedValue>,
    pub page_setup: HashMap<String, OwnedValue>,
}

impl PrintUi {
    pub async fn run(self, proxy: &UiProxy) -> Result<PrintResult, UiError> {
        crate::gui::run_ui_task(
            proxy,
            |send, context, close_on_close| self.run_impl(send, context, close_on_close),
            || UiError::Closed,
        )
        .await
    }

    fn run_impl(
        self,
        send: Sender<Result<PrintResult, UiError>>,
        context: MainContext,
        close_on_close: Receiver<()>,
    ) {
        let dummy_parent = gtk4::Window::new();
        let dialog = PrintUnixDialog::new(Some(&self.title), Some(&dummy_parent));
        dialog.set_modal(true);

        crate::gui::setup_wayland(&dialog, &self.parent_window);

        dialog.connect_response(move |d, r| {
            let res = match r {
                ResponseType::Ok => {
                    let mut settings_map = HashMap::new();
                    let mut page_setup_map = HashMap::new();

                    let settings = d.settings();
                    settings.foreach(|k, v| {
                        if let Ok(owned) =
                            zbus::zvariant::OwnedValue::try_from(zbus::zvariant::Value::from(v))
                        {
                            settings_map.insert(k.to_string(), owned);
                        }
                    });

                    let page_setup = d.page_setup();
                    let key_file = gtk4::glib::KeyFile::new();
                    page_setup.to_key_file(&key_file, Some("Page Setup"));
                    if let Ok(keys) = key_file.keys("Page Setup") {
                        for key in keys {
                            if let Ok(val) = key_file.value("Page Setup", &key) {
                                if let Ok(owned) = zbus::zvariant::OwnedValue::try_from(
                                    zbus::zvariant::Value::from(val.as_str()),
                                ) {
                                    page_setup_map.insert(key.to_string(), owned);
                                }
                            }
                        }
                    }

                    let printer = d.selected_printer();
                    if let Some(printer) = printer {
                        let settings_obj = d.settings();
                        let page_setup_obj = d.page_setup();
                        let token: u32 = rand::random();
                        PRINT_JOBS.with(|jobs| {
                            jobs.borrow_mut().insert(
                                token,
                                CachedPrintJob {
                                    app_id: self.app_id.clone(),
                                    title: self.title.clone(),
                                    printer,
                                    settings: settings_obj,
                                    page_setup: page_setup_obj,
                                },
                            );
                        });

                        Ok(PrintResult {
                            token,
                            settings: settings_map,
                            page_setup: page_setup_map,
                        })
                    } else {
                        // Dialog was confirmed but no printer was selected
                        Err(UiError::Rejected)
                    }
                }
                _ => Err(UiError::Rejected),
            };
            let _ = send.send_blocking(res);
            d.close();
            dummy_parent.destroy();
        });

        dialog.show();
        context.spawn_local(async move {
            let _ = close_on_close.recv().await;
            dialog.close();
        });
    }
}

pub struct ExecutePrintUi {
    pub token: u32,
    pub fd: i32,
}

impl ExecutePrintUi {
    pub async fn run(self, proxy: &UiProxy) -> Result<(), UiError> {
        crate::gui::run_ui_task(proxy, |send, _, _| self.run_impl(send), || UiError::Closed).await
    }

    fn run_impl(self, send: Sender<Result<(), UiError>>) {
        let job = PRINT_JOBS.with(|jobs| jobs.borrow_mut().remove(&self.token));

        if let Some(cached) = job {
            let print_job = gtk4::PrintJob::new(
                &cached.title,
                &cached.printer,
                &cached.settings,
                &cached.page_setup,
            );
            if let Err(e) = print_job.set_source_fd(self.fd) {
                log::error!("Failed to set source fd for print job: {}", e);
                let _ = send.send_blocking(Err(UiError::Rejected));
                return;
            }

            print_job.send(move |_, err| {
                if let Err(e) = err {
                    log::error!("Failed to send print job: {}", e);
                } else {
                    log::info!("Print job successfully sent to CUPS");
                }
            });
            let _ = send.send_blocking(Ok(()));
        } else {
            log::warn!("Received print request for unknown token: {}", self.token);
            let _ = send.send_blocking(Err(UiError::Rejected));
        }
    }
}
