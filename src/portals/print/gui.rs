use {
    crate::gui::{PortalDispatcher, UiError, UiProxy},
    gtk4::{
        PrintUnixDialog, Printer, ResponseType,
        glib::{self, MainContext},
        prelude::{DialogExt, GtkWindowExt, WidgetExt},
    },
    std::{cell::RefCell, collections::HashMap, time::Duration},
    tokio::sync::oneshot::Receiver,
    zbus::zvariant::{OwnedValue, Value},
};

const PRINT_TOKEN_TIMEOUT_SECS: u32 = 300;

pub struct CachedPrintJob {
    pub app_id: String,
    pub title: String,
    pub printer: Printer,
    pub settings: gtk4::PrintSettings,
    pub page_setup: gtk4::PageSetup,
    pub source_id: glib::SourceId,
}

// Since `gtk4::Printer` and related objects are `!Send`, we must cache the print jobs
// on the GTK main thread. When the frontend later calls the `Print` method with a token,
// we retrieve the job from this thread-local map and execute it.
thread_local! {
    pub static PRINT_JOBS: RefCell<HashMap<u32, CachedPrintJob>> = RefCell::new(HashMap::new());
}

pub struct PrintUi {
    pub app_id: String,
    pub parent_window: String,
    pub activation_token: Option<String>,
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
        send: crate::gui::UiDispatcher<Result<PrintResult, UiError>>,
        context: MainContext,
        close_on_close: Receiver<()>,
    ) {
        let dialog = PrintUnixDialog::new(Some(&self.title), None::<&gtk4::Window>);
        dialog.set_modal(true);

        crate::gui::windowing::external_window::setup_window(
            &dialog,
            &self.parent_window,
            self.activation_token.as_deref(),
        );

        let send_clone = send.clone();

        dialog.connect_response(move |d, r| {
            let res = (|| -> Result<PrintResult, UiError> {
                if r != ResponseType::Ok {
                    return Err(UiError::Rejected);
                }

                let mut settings_map = HashMap::new();
                let mut page_setup_map = HashMap::new();

                let settings = d.settings();
                settings.foreach(|k, v| {
                    if let Ok(owned) = zbus::zvariant::OwnedValue::try_from(Value::from(v)) {
                        settings_map.insert(k.to_string(), owned);
                    }
                });

                let page_setup = d.page_setup();
                let key_file = glib::KeyFile::new();
                page_setup.to_key_file(&key_file, Some("Page Setup"));
                if let Ok(keys) = key_file.keys("Page Setup") {
                    for key in keys {
                        let Ok(val) = key_file.value("Page Setup", &key) else {
                            continue;
                        };
                        let Ok(owned) =
                            zbus::zvariant::OwnedValue::try_from(Value::from(val.as_str()))
                        else {
                            continue;
                        };
                        page_setup_map.insert(key.to_string(), owned);
                    }
                }

                let Some(printer) = d.selected_printer() else {
                    // Dialog was confirmed but no printer was selected
                    return Err(UiError::Rejected);
                };

                let settings_obj = d.settings();
                let page_setup_obj = d.page_setup();

                // Generate a random token to identify this job in the subsequent `Print` call.
                let token: u32 = fastrand::u32(..);
                let token_clone = token;

                // The XDG Desktop Portal Print specification expects the application to call `Print`
                // after `PreparePrint` successfully returns a token. We allow a 300-second (5 minute)
                // timeout for the application to generate its print document (e.g. PDF) and call `Print`.
                // If it takes longer or crashes, we evict the cached job to prevent a memory leak.
                let source_id =
                    glib::timeout_add_seconds_local_once(PRINT_TOKEN_TIMEOUT_SECS, move || {
                        PRINT_JOBS.with(|jobs| {
                            jobs.borrow_mut().remove(&token_clone);
                        });
                    });

                PRINT_JOBS.with(|jobs| {
                    jobs.borrow_mut().insert(
                        token,
                        CachedPrintJob {
                            app_id: self.app_id.clone(),
                            title: self.title.clone(),
                            printer,
                            settings: settings_obj,
                            page_setup: page_setup_obj,
                            source_id,
                        },
                    );
                });

                Ok(PrintResult {
                    token,
                    settings: settings_map,
                    page_setup: page_setup_map,
                })
            })();
            let _ = send_clone.dispatch(res);
            d.close();
        });

        dialog.show();
        context.spawn_local(async move {
            let _ = close_on_close.await;
            glib::timeout_future(Duration::from_secs(5)).await;
            dialog.destroy();
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

    fn run_impl(self, send: crate::gui::UiDispatcher<Result<(), UiError>>) {
        let job = PRINT_JOBS.with(|jobs| jobs.borrow_mut().remove(&self.token));

        let Some(cached) = job else {
            tracing::warn!("Received print request for unknown token: {}", self.token);
            let _ = send.dispatch(Err(UiError::Rejected));
            return;
        };

        // Cancel the eviction timeout since we are now executing the print job
        cached.source_id.remove();

        let print_job = gtk4::PrintJob::new(
            &cached.title,
            &cached.printer,
            &cached.settings,
            &cached.page_setup,
        );
        if let Err(e) = print_job.set_source_fd(self.fd) {
            tracing::error!("Failed to set source fd for print job: {}", e);
            let _ = send.dispatch(Err(UiError::Rejected));
            return;
        }

        print_job.send(move |_, err| {
            if let Err(e) = err {
                tracing::error!("Failed to send print job: {}", e);
            } else {
                tracing::info!("Print job successfully sent to CUPS");
            }
        });
        let _ = send.dispatch(Ok(()));
    }
}
