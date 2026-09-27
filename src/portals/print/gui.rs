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
    /// Token -> (owning app_id, job). The owner is kept alongside the job so
    /// `claim_token` can authorise a claim without touching the GTK objects.
    pub static PRINT_JOBS: RefCell<HashMap<u32, (String, CachedPrintJob)>> =
        RefCell::new(HashMap::new());
}

/// Outcome of trying to claim a `PreparePrint` token.
pub enum TokenClaim<T> {
    /// No such token.
    Unknown,
    /// The token exists but belongs to a different application. The entry is
    /// left in place so its rightful owner can still use it.
    WrongOwner(String),
    /// The token belonged to `app_id` and has been consumed.
    Granted(T),
}

/// Claim a print token on behalf of `app_id`.
///
/// A token is only valid for the application that obtained it from
/// `PreparePrint`. Without this check any sandboxed application could consume
/// another application's cached printer, page setup and settings by guessing a
/// token. `xdg-desktop-portal-gtk` performs the same owner check.
pub fn claim_token<T>(
    jobs: &mut HashMap<u32, (String, T)>,
    token: u32,
    app_id: &str,
) -> TokenClaim<T> {
    match jobs.get(&token) {
        None => TokenClaim::Unknown,
        Some((owner, _)) if owner != app_id => TokenClaim::WrongOwner(owner.clone()),
        Some(_) => match jobs.remove(&token) {
            Some((_, job)) => TokenClaim::Granted(job),
            None => TokenClaim::Unknown,
        },
    }
}

pub struct PrintUi {
    pub app_id: String,
    pub parent_window: String,
    pub activation_token: Option<String>,
    pub title: String,
    /// The caller's requested print settings, as the flat key→value dict the
    /// portal contract uses. Applied to the dialog before it is shown, so a
    /// requested printer, paper size or orientation is not silently discarded.
    pub settings: HashMap<String, String>,
    pub page_setup: HashMap<String, String>,
}

const SETTINGS_GROUP: &str = "Print Settings";
const PAGE_SETUP_GROUP: &str = "Page Setup";

/// Rebuild a GTK object from a flat key→value dict.
///
/// The contract carries `settings` and `page_setup` as flat `a{sv}` dicts, and
/// this is the inverse of the serialisation the portal itself produces. GTK4
/// has no GVariant constructors for these (unlike GTK3), so the dict is staged
/// through a `KeyFile` and handed to GTK from there.
fn from_flat_dict(entries: &HashMap<String, String>, group: &str) -> glib::KeyFile {
    let key_file = glib::KeyFile::new();
    for (k, v) in entries {
        key_file.set_string(group, k, v);
    }
    key_file
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

        // Apply what the caller asked for before the dialog is shown, otherwise
        // the user's confirmation is about settings the application never chose.
        if !self.settings.is_empty() {
            let kf = from_flat_dict(&self.settings, SETTINGS_GROUP);
            match gtk4::PrintSettings::from_key_file(&kf, Some(SETTINGS_GROUP)) {
                Ok(s) => dialog.set_settings(Some(&s)),
                Err(e) => tracing::warn!("Could not apply requested print settings: {e}"),
            }
        }
        if !self.page_setup.is_empty() {
            let kf = from_flat_dict(&self.page_setup, PAGE_SETUP_GROUP);
            match gtk4::PageSetup::from_key_file(&kf, Some(PAGE_SETUP_GROUP)) {
                Ok(p) => dialog.set_page_setup(&p),
                Err(e) => tracing::warn!("Could not apply requested page setup: {e}"),
            }
        }

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
                        settings_map.insert(String::from(k), owned);
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
                        page_setup_map.insert(String::from(key.as_str()), owned);
                    }
                }

                let Some(printer) = d.selected_printer() else {
                    // Dialog was confirmed but no printer was selected
                    return Err(UiError::Rejected);
                };

                let settings_obj = d.settings();
                let page_setup_obj = d.page_setup();

                // Generate a token to identify this job in the subsequent
                // `Print` call. Zero is reserved by the contract as "no token",
                // and a token must not collide with one already cached, or a
                // later `Print` would silently pick up the wrong job's printer
                // and settings. GTK3 retries on both conditions.
                let token: u32 = PRINT_JOBS.with(|jobs| {
                    let jobs = jobs.borrow();
                    let mut candidate = fastrand::u32(1..);
                    while jobs.contains_key(&candidate) {
                        candidate = fastrand::u32(1..);
                    }
                    candidate
                });
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
                        (
                            self.app_id.clone(),
                            CachedPrintJob {
                                app_id: self.app_id.clone(),
                                title: self.title.clone(),
                                printer,
                                settings: settings_obj,
                                page_setup: page_setup_obj,
                                source_id,
                            },
                        ),
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
    /// The application submitting the job. A token is only valid for the
    /// application that obtained it from `PreparePrint`.
    pub app_id: String,
    pub fd: i32,
}

impl ExecutePrintUi {
    pub async fn run(self, proxy: &UiProxy) -> Result<(), UiError> {
        crate::gui::run_ui_task(proxy, |send, _, _| self.run_impl(send), || UiError::Closed).await
    }

    fn run_impl(self, send: crate::gui::UiDispatcher<Result<(), UiError>>) {
        // Authorise and consume the token in one step, so a refused call cannot
        // consume another application's job: `claim_token` leaves a wrong-owner
        // token in place for its rightful owner.
        let cached = match PRINT_JOBS
            .with(|jobs| claim_token(&mut jobs.borrow_mut(), self.token, &self.app_id))
        {
            TokenClaim::Granted(job) => job,
            TokenClaim::Unknown => {
                tracing::warn!("Received print request for unknown token: {}", self.token);
                let _ = send.dispatch(Err(UiError::Rejected));
                return;
            }
            TokenClaim::WrongOwner(owner) => {
                tracing::warn!(
                    "Token {} belongs to {owner}, refusing to print it for {}",
                    self.token,
                    self.app_id
                );
                let _ = send.dispatch(Err(UiError::Rejected));
                return;
            }
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
