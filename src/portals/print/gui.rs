use std::os::fd::{AsRawFd, OwnedFd};

use {
    crate::gui::{PortalDispatcher, UiError, UiProxy},
    gtk4::{
        PrintUnixDialog, Printer, ResponseType,
        glib::{self, MainContext},
        prelude::{DialogExt, GtkWindowExt, WidgetExt},
    },
    std::{cell::RefCell, collections::HashMap, sync::Arc, time::Duration},
    tokio::sync::oneshot::Receiver,
    zbus::zvariant::{OwnedValue, Value},
};

const PRINT_TOKEN_TIMEOUT_SECS: u32 = 300;

/// A print job parked between `PreparePrint` and `Print`.
///
/// Deliberately carries no `app_id`: ownership lives in the `PRINT_JOBS` value
/// tuple, which is what `claim_token` authorises against. A copy of `app_id`
/// inside the job was write-only, costing a `String` allocation and 24 bytes of
/// struct for a value no reader could observe.
pub struct CachedPrintJob {
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
    ///
    /// The owner is an `Arc<str>` so that reporting a wrong-owner refusal
    /// hands back a refcount bump instead of a fresh `String` allocation.
    pub static PRINT_JOBS: RefCell<HashMap<u32, (Arc<str>, CachedPrintJob)>> =
        RefCell::new(HashMap::new());
}

/// Outcome of trying to claim a `PreparePrint` token.
#[derive(Debug)]
pub enum TokenClaim<T> {
    /// No such token.
    Unknown,
    /// The token exists but belongs to a different application. The entry is
    /// left in place so its rightful owner can still use it.
    WrongOwner(Arc<str>),
    /// The token belonged to `app_id` and has been consumed.
    Granted(T),
}

/// Claim a print token on behalf of `app_id`.
///
/// A token is only valid for the application that obtained it from
/// `PreparePrint`. Without this check any sandboxed application could consume
/// another application's cached printer, page setup and settings by guessing a
/// token, so ownership is part of what makes the token safe to hand out.
pub fn claim_token<T>(
    jobs: &mut HashMap<u32, (Arc<str>, T)>,
    token: u32,
    app_id: &str,
) -> TokenClaim<T> {
    match jobs.get(&token) {
        None => TokenClaim::Unknown,
        Some((owner, _)) if owner.as_ref() != app_id => TokenClaim::WrongOwner(owner.clone()),
        Some(_) => match jobs.remove(&token) {
            Some((_, job)) => TokenClaim::Granted(job),
            None => TokenClaim::Unknown,
        },
    }
}

/// Non-consuming lookup used by [`PrintTokenProbe`].
///
/// Generic over the job payload so it can be exercised in tests without
/// constructing the `!Send` GTK objects held by the real cache.
fn token_is_cached<T>(jobs: &HashMap<u32, (Arc<str>, T)>, token: u32) -> bool {
    jobs.contains_key(&token)
}

/// Reports whether `token` is parked in the `PRINT_JOBS` cache, without claiming it.
///
/// `Print` uses this to tell a job prepared by an earlier `PreparePrint` from a
/// caller (such as `GtkPrintDialog` with no setup) that expects the dialog to be
/// shown at print time. Ownership is still checked by [`claim_token`] when the
/// job is executed, so probing never authorises a foreign token.
pub struct PrintTokenProbe {
    pub token: u32,
}

impl PrintTokenProbe {
    pub async fn run(self, proxy: &UiProxy) -> Result<bool, UiError> {
        crate::gui::run_ui_task(
            proxy,
            move |send, _, _| {
                let exists = PRINT_JOBS.with(|jobs| token_is_cached(&jobs.borrow(), self.token));
                let _ = send.dispatch(Ok(exists));
            },
            || UiError::Closed,
        )
        .await
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
                            Arc::from(self.app_id.as_str()),
                            CachedPrintJob {
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

/// Owns the duplicate of the print source fd until the GTK job is done.
///
/// GTK never closes a descriptor passed to `set_source_fd`, and gtk-rs hands
/// `print_job.send()` an `Fn` callback, so the close is an idempotent
/// `Option::take()` performed from that callback.
struct JobFd(RefCell<Option<OwnedFd>>);

impl JobFd {
    fn new(fd: OwnedFd) -> Self {
        Self(RefCell::new(Some(fd)))
    }

    /// Close the descriptor. Safe to call more than once.
    fn close(&self) {
        self.0.borrow_mut().take();
    }
}

pub struct ExecutePrintUi {
    pub token: u32,
    /// The application submitting the job. A token is only valid for the
    /// application that obtained it from `PreparePrint`.
    pub app_id: String,
    /// An owned duplicate of the fd supplied with `Print`. GTK does not close
    /// descriptors handed to `set_source_fd`, so this copy is closed by the
    /// job's completion callback.
    pub fd: OwnedFd,
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
                let _ = send.dispatch(Err(UiError::Refused));
                return;
            }
            TokenClaim::WrongOwner(owner) => {
                tracing::warn!(
                    "Token {} belongs to {owner}, refusing to print it for {}",
                    self.token,
                    self.app_id
                );
                let _ = send.dispatch(Err(UiError::Refused));
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
        if let Err(e) = print_job.set_source_fd(self.fd.as_raw_fd()) {
            tracing::error!("Failed to set source fd for print job: {}", e);
            let _ = send.dispatch(Err(UiError::Refused));
            return; // self.fd is dropped here, closing our copy
        }

        // Keep our copy alive until GTK is done reading it, then close it.
        // gtk-rs drops the boxed `Fn` callback via its destroy notify when the
        // job completes, so the fd is closed exactly once on that path too.
        let source_fd = JobFd::new(self.fd);
        print_job.send(move |_, err| {
            source_fd.close();
            match err {
                Ok(()) => {
                    tracing::info!("Print job successfully sent to CUPS");
                    let _ = send.dispatch(Ok(()));
                }
                Err(e) => {
                    tracing::error!("Failed to send print job: {}", e);
                    let _ = send.dispatch(Err(UiError::Failed));
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `CachedPrintJob` holds `!Send` GTK objects, so these exercise
    /// `claim_token` with a plain payload. The ownership logic does not look at
    /// the payload, so this covers the same branches the real caller hits.
    type Jobs = HashMap<u32, (Arc<str>, u32)>;

    fn jobs_with(token: u32, owner: &str, payload: u32) -> Jobs {
        let mut jobs = Jobs::new();
        jobs.insert(token, (Arc::from(owner), payload));
        jobs
    }

    #[test]
    fn claim_unknown_token_is_rejected() {
        let mut jobs = jobs_with(7, "org.example.A", 99);
        let claim = claim_token(&mut jobs, 8, "org.example.A");
        assert!(matches!(claim, TokenClaim::Unknown));
        // The unrelated entry must survive.
        assert_eq!(jobs.len(), 1);
    }

    /// The fd copy must outlive GTK's asynchronous read and be closed exactly
    /// once when the job completes, even if the callback fires twice.
    #[test]
    fn job_fd_closes_the_descriptor_exactly_once() {
        use std::os::fd::AsFd;

        let file = tempfile::tempfile().unwrap();
        let dup = rustix::io::dup(file.as_fd()).unwrap();
        let raw = dup.as_raw_fd();

        assert!(
            unsafe { libc::fcntl(raw, libc::F_GETFD) } >= 0,
            "the descriptor must be open while the job owns it"
        );

        let job_fd = JobFd::new(dup);
        job_fd.close();
        assert_eq!(
            unsafe { libc::fcntl(raw, libc::F_GETFD) },
            -1,
            "the descriptor must be closed after the callback"
        );
        job_fd.close(); // a second callback must not close anything again
    }

    /// Exercise the `run_ui_task` wrapper itself, not just its inner lookup:
    /// the probe must dispatch a real boolean and leave the cache untouched.
    #[tokio::test]
    async fn token_probe_dispatches_a_real_answer() {
        use crate::gui::UiProxy;

        PRINT_JOBS.with(|jobs| jobs.borrow_mut().clear());
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let proxy = UiProxy {
            context: gtk4::glib::MainContext::default(),
            sender,
        };

        let probe = tokio::spawn(async move { PrintTokenProbe { token: 7 }.run(&proxy).await });
        let task = receiver
            .recv()
            .await
            .expect("the probe must be queued for the GTK thread");
        task();

        assert!(!probe.await.unwrap().unwrap(), "token 7 is not cached");
        assert!(
            PRINT_JOBS.with(|jobs| jobs.borrow().is_empty()),
            "a probe must not insert or remove cache entries"
        );
    }

    /// The `Print` call probes before it claims. A probe must leave the job in
    /// place so the subsequent claim (or the rightful owner's claim) still
    /// succeeds.
    #[test]
    fn probing_a_token_does_not_consume_it() {
        let mut jobs = jobs_with(7, "org.example.A", 99);
        assert!(token_is_cached(&jobs, 7));
        assert!(!token_is_cached(&jobs, 8));
        assert_eq!(jobs.len(), 1, "a probe must not remove the cached job");
        assert!(matches!(
            claim_token(&mut jobs, 7, "org.example.A"),
            TokenClaim::Granted(99)
        ));
    }

    #[test]
    fn claim_by_the_owning_app_is_granted_and_consumes_the_entry() {
        let mut jobs = jobs_with(7, "org.example.A", 99);
        let claim = claim_token(&mut jobs, 7, "org.example.A");
        match claim {
            TokenClaim::Granted(payload) => assert_eq!(payload, 99),
            other => panic!("expected Granted, got {other:?}"),
        }
        assert!(jobs.is_empty(), "a granted token must not be reusable");
    }

    #[test]
    fn claim_by_a_different_app_is_refused_and_leaves_the_entry() {
        let mut jobs = jobs_with(7, "org.example.A", 99);
        let claim = claim_token(&mut jobs, 7, "org.example.Evil");
        match claim {
            TokenClaim::WrongOwner(owner) => assert_eq!(&*owner, "org.example.A"),
            other => panic!("expected WrongOwner, got {other:?}"),
        }
        // The rightful owner must still be able to use it.
        assert_eq!(jobs.len(), 1, "a refused claim must not consume the token");
        assert!(matches!(
            claim_token(&mut jobs, 7, "org.example.A"),
            TokenClaim::Granted(99)
        ));
    }

    #[test]
    fn owner_comparison_is_exact_not_prefix() {
        let mut jobs = jobs_with(7, "org.example.A", 1);
        // A prefix of the real owner must not be accepted.
        assert!(matches!(
            claim_token(&mut jobs, 7, "org.example"),
            TokenClaim::WrongOwner(_)
        ));
        // Nor a superstring.
        assert!(matches!(
            claim_token(&mut jobs, 7, "org.example.A.evil"),
            TokenClaim::WrongOwner(_)
        ));
        assert_eq!(jobs.len(), 1);
    }

    #[test]
    fn wrong_owner_report_does_not_allocate() {
        let mut jobs = jobs_with(7, "org.example.Owner", 1);
        let scope = crate::alloc_probe::AllocScope::start();
        let claim = claim_token(&mut jobs, 7, "org.example.Intruder");
        let snap = scope.finish();
        assert!(matches!(claim, TokenClaim::WrongOwner(_)));
        assert_eq!(
            snap.count, 0,
            "reporting the owner should be a refcount bump, got {snap:?}",
        );
    }

    #[test]
    fn empty_owner_is_not_a_wildcard() {
        // A job cached under an empty owner must not be claimable by an empty
        // caller unless the owner really is empty.
        let mut jobs = jobs_with(1, "", 5);
        assert!(matches!(
            claim_token(&mut jobs, 1, ""),
            TokenClaim::Granted(5)
        ));
    }
}
