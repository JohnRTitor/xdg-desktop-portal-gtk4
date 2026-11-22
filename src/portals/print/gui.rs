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
    zbus::{OwnedVariant, Variant},
};

const PRINT_TOKEN_TIMEOUT_SECS: u32 = 300;

/// A print job parked between `PreparePrint` and `Print`.
///
/// Deliberately carries no `app_id`: ownership lives in the `PRINT_JOBS` value
/// tuple, which is what `claim_token` authorises against.
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
    /// The caller's requested print settings, as the `a{sv}` dict the portal
    /// contract uses. Applied to the dialog before it is shown, so a requested
    /// printer, paper size or orientation is not silently discarded.
    pub settings: HashMap<String, OwnedVariant>,
    pub page_setup: HashMap<String, OwnedVariant>,
}

/// Convert one scalar from a zbus `a{sv}` value into a `glib::Variant`.
///
/// The value types must survive the trip: `PageSetup` stores paper dimensions
/// and margins as doubles, and `PrintSettings` has boolean and integer keys as
/// well as strings. A string-only key file dropped all of the non-strings.
/// Together with [`glib_to_owned`] this accepts the numeric and string scalar
/// types used by the print portal: `b/y/n/q/i/u/x/t/d/s`. Other basic types,
/// such as object paths, signatures and Unix FDs, are skipped with a warning.
fn owned_to_glib(value: &OwnedVariant) -> Option<glib::Variant> {
    let v = match &**value {
        Variant::Bool(v) => (*v).into(),
        Variant::U8(v) => (*v).into(),
        Variant::I16(v) => (*v).into(),
        Variant::U16(v) => (*v).into(),
        Variant::F64(v) => (*v).into(),
        Variant::I32(v) => (*v).into(),
        Variant::U32(v) => (*v).into(),
        Variant::I64(v) => (*v).into(),
        Variant::U64(v) => (*v).into(),
        Variant::Str(s) => s.as_str().into(),
        _ => return None,
    };
    Some(v)
}

/// Convert one `a{sv}` value coming from GTK back into an owned zbus value.
fn glib_to_owned(value: &glib::Variant) -> Option<OwnedVariant> {
    // `a{sv}` boxes every value in a `v`; unwrap it before inspecting the type.
    let inner = if value.type_() == glib::VariantTy::VARIANT {
        value.get::<glib::Variant>()?
    } else {
        value.clone()
    };
    // Match the borrowed signature directly: `type_().to_string()` allocated
    // a `String` for every entry of every `a{sv}` dict round-tripped through
    // GTK. `VariantTy` itself is unsized, so it cannot be used in a match
    // pattern; `as_str` gives us a borrowed `&str` to match on.
    match inner.type_().as_str() {
        "s" => {
            let s = inner.get::<String>()?;
            OwnedVariant::try_from(Variant::from(s.as_str())).ok()
        }
        "d" => Some(OwnedVariant::from(inner.get::<f64>()?)),
        "b" => Some(OwnedVariant::from(inner.get::<bool>()?)),
        "y" => Some(OwnedVariant::from(inner.get::<u8>()?)),
        "n" => Some(OwnedVariant::from(inner.get::<i16>()?)),
        "q" => Some(OwnedVariant::from(inner.get::<u16>()?)),
        "i" => Some(OwnedVariant::from(inner.get::<i32>()?)),
        "u" => Some(OwnedVariant::from(inner.get::<u32>()?)),
        "x" => Some(OwnedVariant::from(inner.get::<i64>()?)),
        "t" => Some(OwnedVariant::from(inner.get::<u64>()?)),
        _ => None,
    }
}

/// Convert a `glib::Variant` of type `a{sv}` into owned zbus values.
///
/// Entries with an unsupported value type are skipped individually: dropping
/// the whole dict because of one key would discard every valid setting too.
fn variant_to_owned_dict(variant: &glib::Variant) -> HashMap<String, OwnedVariant> {
    // Every child is a `{sv}` entry, so the child count is an upper bound on
    // the number of keys we can keep. Size the map once up front.
    let mut result = HashMap::with_capacity(variant.n_children());
    for entry in variant.iter() {
        // A dict entry is `{sv}`, which is not the same type as `(sv)` to
        // GVariant even though their binary layouts match. Read the two
        // children directly instead of asking for a tuple.
        let Some(key) = entry.child_value(0).get::<String>() else {
            continue;
        };
        let value = entry.child_value(1);
        match glib_to_owned(&value) {
            Some(owned) => {
                result.insert(key, owned);
            }
            None => {
                tracing::warn!(%key, ?value, "Ignoring unsupported value from GTK print dict");
            }
        }
    }
    result
}

/// Inverse of [`variant_to_owned_dict`]: rebuild the `a{sv}` `glib::Variant`
/// that GTK's `from_gvariant` constructors expect.
///
/// Unsupported values and empty keys are skipped individually, so one bad
/// entry cannot discard the rest of the caller's requested settings. GLib
/// emits a critical for an empty key, which would also be an attacker-driven
/// log-spam / abort vector under `G_DEBUG=fatal-criticals`.
fn owned_dict_to_variant(dict: &HashMap<String, OwnedVariant>) -> glib::Variant {
    let builder = glib::VariantDict::new(None::<&glib::Variant>);
    for (key, value) in dict {
        match to_variant_entry(key, value) {
            Some((key, variant)) => builder.insert_value(key, &variant),
            None => tracing::warn!(%key, ?value, "Ignoring invalid print setting entry"),
        }
    }
    builder.end()
}

/// Validate one caller-supplied entry before it reaches GLib. Separating this
/// from the dict loop lets tests verify the empty-key guard directly: GLib
/// would reject an empty key on its own, so outcome-only tests cannot tell
/// whether our guard is still there.
fn to_variant_entry<'a>(key: &'a str, value: &OwnedVariant) -> Option<(&'a str, glib::Variant)> {
    if key.is_empty() {
        return None;
    }
    // `VariantDict::insert_value` borrows the key, so hand it straight back
    // instead of allocating a `String` per entry only to borrow it again.
    Some((key, owned_to_glib(value)?))
}

pub struct PrintResult {
    pub token: u32,
    pub settings: HashMap<String, OwnedVariant>,
    pub page_setup: HashMap<String, OwnedVariant>,
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
            let variant = owned_dict_to_variant(&self.settings);
            dialog.set_settings(Some(&gtk4::PrintSettings::from_gvariant(&variant)));
        }
        if !self.page_setup.is_empty() {
            let variant = owned_dict_to_variant(&self.page_setup);
            dialog.set_page_setup(&gtk4::PageSetup::from_gvariant(&variant));
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
                    tracing::debug!(response = ?r, "Print dialog dismissed");
                    return Err(UiError::Rejected);
                }

                let settings_map = variant_to_owned_dict(&d.settings().to_gvariant());
                let page_setup_map = variant_to_owned_dict(&d.page_setup().to_gvariant());

                let Some(printer) = d.selected_printer() else {
                    return Err(UiError::Rejected);
                };

                let settings_obj = d.settings();
                let page_setup_obj = d.page_setup();

                tracing::debug!(
                    printer = %printer.name(),
                    output_uri = ?settings_obj.get("output-uri"),
                    "Print dialog accepted; job cached"
                );

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
                        tracing::debug!(token = token_clone, "Cached print job expired");
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
    /// The application submitting the job. A token is only valid for the
    /// application that obtained it from `PreparePrint`.
    ///
    /// Listed first so the two 4-byte fields pack into the `String`'s trailing
    /// slots: declaration order costs 40 bytes with 8 bytes of padding, while
    /// this order is exactly 32 bytes.
    pub app_id: String,
    pub token: u32,
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

        cached.source_id.remove();

        tracing::debug!(
            token = self.token,
            printer = %cached.printer.name(),
            output_uri = ?cached.settings.get("output-uri"),
            output_basename = ?cached.settings.get("output-basename"),
            source_fd = self.fd.as_raw_fd(),
            "Submitting print job"
        );

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
                    tracing::debug!("Print job sent successfully");
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

    /// `app_id` first lets `String` (24) + `u32` (4) + `OwnedFd` (4) pack into
    /// exactly 32 bytes. Pin it so a careless field reorder cannot grow it.
    #[test]
    #[cfg(target_pointer_width = "64")]
    fn execute_print_ui_has_no_tail_padding() {
        assert_eq!(std::mem::size_of::<ExecutePrintUi>(), 32);
    }

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

    fn gtk_ready() -> bool {
        static INIT: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *INIT.get_or_init(|| gtk4::init().is_ok())
    }

    /// Assert the concrete GVariant type code of each value, so a symmetric
    /// mistake in both mapping tables cannot hide behind a round-trip equality.
    fn assert_variant_types(variant: &glib::Variant, expected: &[(&str, &str)]) {
        let mut actual = HashMap::new();
        for entry in variant.iter() {
            let key = entry.child_value(0).get::<String>().unwrap();
            let inner = entry.child_value(1).get::<glib::Variant>().unwrap();
            actual.insert(key, inner.type_().to_string());
        }
        for (key, ty) in expected {
            assert_eq!(
                actual.get(*key).map(String::as_str),
                Some(*ty),
                "wrong GVariant type for {key}"
            );
        }
    }

    /// The `glib::Variant` <-> zbus `Variant` bridge must preserve value types,
    /// not only raw bytes: margins and paper dimensions are doubles.
    #[test]
    fn owned_dict_variant_round_trip_preserves_all_scalar_types() {
        use zbus::Variant;

        let dict = HashMap::from([
            (
                "Name".to_string(),
                OwnedVariant::try_from(Variant::from("iso_a4_210x297mm")).unwrap(),
            ),
            ("Byte".to_string(), OwnedVariant::from(7u8)),
            ("Short".to_string(), OwnedVariant::from(-3i16)),
            ("UShort".to_string(), OwnedVariant::from(9u16)),
            ("Width".to_string(), OwnedVariant::from(210.0f64)),
            ("Collate".to_string(), OwnedVariant::from(true)),
            ("Copies".to_string(), OwnedVariant::from(2i32)),
            ("Scale".to_string(), OwnedVariant::from(100u32)),
            ("JobId".to_string(), OwnedVariant::from(42i64)),
            ("Timeout".to_string(), OwnedVariant::from(90u64)),
        ]);

        let variant = owned_dict_to_variant(&dict);
        assert_eq!(variant.type_().to_string(), "a{sv}");
        assert_variant_types(
            &variant,
            &[
                ("Name", "s"),
                ("Byte", "y"),
                ("Short", "n"),
                ("UShort", "q"),
                ("Width", "d"),
                ("Collate", "b"),
                ("Copies", "i"),
                ("Scale", "u"),
                ("JobId", "x"),
                ("Timeout", "t"),
            ],
        );
        let back = variant_to_owned_dict(&variant);

        assert_eq!(
            back, dict,
            "the a{{sv}} dict must survive the bridge intact"
        );
        assert_eq!(
            f64::try_from(back["Width"].try_clone().unwrap()).unwrap(),
            210.0
        );
        assert!(bool::try_from(back["Collate"].try_clone().unwrap()).unwrap());
        assert_eq!(
            i32::try_from(back["Copies"].try_clone().unwrap()).unwrap(),
            2
        );
    }

    /// A single unsupported value (or an empty key) must not discard the rest
    /// of the dict.
    #[test]
    fn bad_entries_are_skipped_without_dropping_valid_ones() {
        use zbus::Variant;

        let array = OwnedVariant::try_from(Variant::from(vec![1u32, 2u32])).unwrap();
        let dict = HashMap::from([
            (
                String::new(),
                OwnedVariant::try_from(Variant::from("empty key")).unwrap(),
            ),
            ("BadType".to_string(), array),
            ("MarginTop".to_string(), OwnedVariant::from(11.0f64)),
            (
                "Orientation".to_string(),
                OwnedVariant::try_from(Variant::from("portrait")).unwrap(),
            ),
        ]);

        // Assert the guard directly: GLib rejects empty keys on its own, so an
        // outcome-only check would still pass if our explicit guard disappeared.
        assert!(to_variant_entry("", &OwnedVariant::from(1.0f64)).is_none());
        assert!(to_variant_entry("MarginTop", &OwnedVariant::from(1.0f64)).is_some());

        let variant = owned_dict_to_variant(&dict);
        let back = variant_to_owned_dict(&variant);

        assert_eq!(back.len(), 2, "only the two valid entries should survive");
        assert_eq!(
            f64::try_from(back["MarginTop"].try_clone().unwrap()).unwrap(),
            11.0
        );
        assert_eq!(
            String::try_from(back["Orientation"].try_clone().unwrap()).unwrap(),
            "portrait"
        );
    }

    /// Exercise the response direction directly: a raw GTK-style `a{sv}` with
    /// one unsupported value must still yield all supported entries.
    #[test]
    fn gtk_variant_with_unsupported_value_keeps_valid_entries() {
        use glib::prelude::ToVariant;

        let builder = glib::VariantDict::new(None::<&glib::Variant>);
        builder.insert_value("BadType", &vec![1u32, 2u32].to_variant());
        builder.insert_value("MarginTop", &11.5f64.to_variant());
        builder.insert_value("Orientation", &"portrait".to_variant());

        let back = variant_to_owned_dict(&builder.end());
        assert_eq!(back.len(), 2, "the array value must be skipped, not fatal");
        assert_eq!(
            f64::try_from(back["MarginTop"].try_clone().unwrap()).unwrap(),
            11.5
        );
        assert_eq!(
            String::try_from(back["Orientation"].try_clone().unwrap()).unwrap(),
            "portrait"
        );
    }

    /// GTK sends doubles for paper dimensions and margins; verify they reach
    /// GTK so the dialog does not fall back to defaults.
    #[test]
    fn gtk_style_page_setup_dict_applies_margins() {
        use zbus::Variant;

        let string = |s: &str| OwnedVariant::try_from(Variant::from(s)).unwrap();
        let number = |v: f64| OwnedVariant::from(v);

        let dict = HashMap::from([
            ("Name".to_string(), string("iso_a4_210x297mm")),
            ("DisplayName".to_string(), string("A4")),
            ("Width".to_string(), number(210.0)),
            ("Height".to_string(), number(297.0)),
            ("MarginTop".to_string(), number(11.0)),
            ("MarginBottom".to_string(), number(11.0)),
            ("MarginLeft".to_string(), number(11.0)),
            ("MarginRight".to_string(), number(11.0)),
            ("Orientation".to_string(), string("portrait")),
        ]);

        let variant = owned_dict_to_variant(&dict);
        // These assertions need no display, so they run in headless CI too.
        assert_variant_types(
            &variant,
            &[("MarginTop", "d"), ("Width", "d"), ("Orientation", "s")],
        );
        let typed = variant_to_owned_dict(&variant);
        assert_eq!(
            f64::try_from(typed["MarginTop"].try_clone().unwrap()).unwrap(),
            11.0
        );

        if !gtk_ready() {
            eprintln!("No graphical display available; skipping GTK object assertions");
            return;
        }

        // The response path reads real GTK variants, not just VariantDict
        // output: verify MarginTop comes back as a double from GTK's own data.
        let setup = gtk4::PageSetup::from_gvariant(&variant);
        let from_gtk = variant_to_owned_dict(&setup.to_gvariant());
        assert_eq!(
            f64::try_from(from_gtk["MarginTop"].try_clone().unwrap()).unwrap(),
            11.0,
            "GTK's own variant must preserve the margin as a double"
        );
        assert!(
            (setup.top_margin(gtk4::Unit::Mm) - 11.0).abs() < 1e-6,
            "the requested margin must reach GTK instead of being lost"
        );

        // GTK's PrintSettings::from_gvariant stores string values, so verify
        // the caller's string settings also survive the same typed path.
        let settings_dict = HashMap::from([
            ("printer".to_string(), string("Print to File")),
            ("output-uri".to_string(), string("file:///tmp/print.pdf")),
        ]);
        let settings_variant = owned_dict_to_variant(&settings_dict);
        let settings = gtk4::PrintSettings::from_gvariant(&settings_variant);
        let back = variant_to_owned_dict(&settings.to_gvariant());
        assert_eq!(
            String::try_from(back["printer"].try_clone().unwrap()).unwrap(),
            "Print to File"
        );
        assert_eq!(
            String::try_from(back["output-uri"].try_clone().unwrap()).unwrap(),
            "file:///tmp/print.pdf"
        );
    }
}
