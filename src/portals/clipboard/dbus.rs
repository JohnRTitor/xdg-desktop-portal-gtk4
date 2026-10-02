//! D-Bus implementation of the Clipboard portal.
//!
//! This module coordinates clipboard access between sandboxed applications and the host GTK
//! environment. It heavily relies on passing file descriptors (FDs) over D-Bus to stream
//! clipboard content without buffering large amounts of data in the portal memory.
//!
//! # Threading Model
//! The D-Bus interface methods are executed by zbus on Tokio threads. However, clipboard
//! interaction strictly requires GTK main thread access. Thus, requests are often routed
//! through `UiProxy` to the GTK thread.

use {
    crate::{
        gui::{PortalDispatcher, UiProxy},
        portals::clipboard::gtk_backend,
    },
    gtk4::glib::MainContext,
    parking_lot::Mutex,
    std::{
        collections::HashMap,
        os::fd::OwnedFd,
        sync::{
            Arc,
            atomic::{AtomicU32, Ordering},
        },
        time::Duration,
    },
    tokio::sync::{
        Notify,
        oneshot::{Sender, channel},
    },
    zbus::{
        Connection, fdo, interface,
        message::Header,
        object_server::SignalEmitter,
        zvariant::{Fd, ObjectPath, Value},
    },
};

struct TransferRequest {
    fd_sender: Sender<OwnedFd>,
}

/// Per-session serial counters, so one application cannot predict or collide
/// with another application's transfer serials.
///
/// Entries are created lazily by [`next_serial`] the first time a session calls
/// `SetSelection`, and are removed by [`forget_session`] when the session is
/// torn down. The key is an `Arc<str>` so teardown hands the same allocation to
/// the removal path instead of rebuilding it.
static SERIALS: std::sync::LazyLock<Mutex<HashMap<Arc<str>, Arc<AtomicU32>>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

fn next_serial(session: &str) -> u32 {
    let mut counters = SERIALS.lock();
    // `HashMap::entry` needs an owned key, so `Arc::from(session)` would allocate
    // on every call. `Arc<str>: Borrow<str>` makes `get` accept a borrowed key, so
    // a session that already has a counter costs a refcount bump and nothing
    // else. `next_serial` runs once per host clipboard request.
    let counter = match counters.get(session) {
        Some(existing) => existing.clone(),
        None => counters
            .entry(Arc::from(session))
            .or_insert_with(|| Arc::new(AtomicU32::new(1)))
            .clone(),
    };
    let serial = counter.fetch_add(1, Ordering::SeqCst);
    // Skip 0, which the spec reserves as "no serial".
    if serial == 0 {
        counter.fetch_add(1, Ordering::SeqCst)
    } else {
        serial
    }
}

/// Drops the serial counter for a session that no longer exists.
///
/// Without this the map grew without bound: `SERIALS` is a process-lifetime
/// `static`, and clipboard session handles are minted fresh per session, so a
/// long-lived client that opened many sessions in sequence leaked one `Arc<str>`
/// plus one `Arc<AtomicU32>` per session for the lifetime of the daemon.
///
/// Only safe to call once the session is finished: a still-live `SetSelection`
/// task for the same handle would restart the namespace at 1 and reuse a serial.
/// See the comment in `set_selection`.
fn forget_session(session: &str) -> bool {
    SERIALS.lock().remove(session).is_some()
}

/// Builds the `SelectionOwnerChanged` options dict.
///
/// Shared by the host-change broadcast and by `RequestClipboard`, which used to
/// carry two copies of this. The payload depends only on the advertised mime
/// types, so a caller fanning out to several sessions must build it once and
/// clone the map rather than rebuilding it per session.
fn owner_changed_options<'a>(mimes: &'a gtk_backend::MimeList) -> HashMap<&'static str, Value<'a>> {
    let mut options = HashMap::new();
    // `Box<str>` has no `zvariant` `Type` impl, so the array is built from
    // borrowed `&str`s pointing into the list rather than from owned copies.
    options.insert(
        "mime_types",
        Value::from(mimes.iter().map(|m| m.as_ref()).collect::<Vec<&str>>()),
    );
    options.insert("session_is_owner", Value::from(false));
    options
}

/// Extracts the advertised mime types from a `SetSelection` options dict.
///
/// The inverse of [`owner_changed_options`]'s mime encoding.
fn parse_mime_types(options: &HashMap<&str, Value<'_>>) -> gtk_backend::MimeList {
    let mut mimes = gtk_backend::MimeList::new();
    if let Some(Value::Array(arr)) = options.get("mime_types") {
        for i in 0..arr.len() {
            if let Ok(Some(Value::Str(s))) = arr.get::<Value<'_>>(i) {
                mimes.push(s.as_str().into());
            }
        }
    }
    mimes
}

/// Number of live serial counters. Used by the tests to assert on growth.
#[cfg(test)]
fn tracked_serial_count() -> usize {
    SERIALS.lock().len()
}

/// D-Bus interface wrapper for the Clipboard portal.
///
/// This struct holds the shared state for clipboard operations, notably managing
/// the active sessions (which need to be notified of host clipboard changes) and
/// pending file descriptor transfers.
pub struct ClipboardPortal {
    /// Tracks active sessions that have requested clipboard access.
    /// We emit `SelectionOwnerChanged` signals to all these sessions when the host clipboard changes.
    active_sessions: Arc<Mutex<Vec<ObjectPath<'static>>>>,

    /// Maps a `(session, serial)` pair to an active transfer request.
    ///
    /// When the host wants to read from a sandboxed app, we generate a serial
    /// scoped to that session, pass it to the app via `SelectionTransfer`, and
    /// when the app calls `SelectionWrite` with the same session and serial we
    /// map it back to the `fd_sender` to provide the writing end of a pipe.
    ///
    /// The session is part of the key on purpose. Serials are a per-session
    /// namespace, and validating the session separately is not sufficient on its
    /// own: a caller that passed an unknown session but a serial belonging to
    /// someone else must still not receive that session's descriptor.
    pending_transfers: Arc<Mutex<HashMap<(String, u32), TransferRequest>>>,

    connection: Connection,
    proxy: UiProxy,
    session_manager: crate::core::session_manager::SessionManager,
}

impl ClipboardPortal {
    pub fn new(
        connection: Connection,
        proxy: UiProxy,
        session_manager: crate::core::session_manager::SessionManager,
    ) -> Self {
        let pending_transfers = Arc::new(Mutex::new(HashMap::new()));
        let active_sessions = Arc::new(Mutex::new(Vec::new()));

        let conn_clone = connection.clone();
        let sessions_clone = active_sessions.clone();

        let (tx, rx) = channel();

        // Run GTK-specific initialization on the main thread and pipe the event stream back to Tokio
        let _ = proxy.sender.send(Box::new(move || {
            MainContext::default().spawn_local(async move {
                match gtk_backend::subscribe_changes() {
                    Ok(formats_rx) => {
                        let _ = tx.send(formats_rx);
                    }
                    Err(e) => {
                        tracing::warn!("Clipboard portal backend unavailable: {}", e);
                    }
                }
            });
        }));

        // Process GTK events and emit D-Bus signals entirely on the Tokio background thread
        // to avoid bogging down the GTK main loop. The GTK thread sends us updates via `rx`.
        tokio::spawn(async move {
            let Ok(mut formats_rx) = rx.await else {
                return;
            };

            while let Ok(mimes) = formats_rx.recv().await {
                let emitter = match SignalEmitter::new(&conn_clone, crate::core::DBUS_PATH) {
                    Ok(e) => e,
                    Err(err) => {
                        tracing::error!("Failed to create SignalEmitter: {}", err);
                        return;
                    }
                };

                // Built once per clipboard change rather than once per session: the
                // payload is identical for every session, so only the
                // per-session projection into the signal's reference-typed
                // map below is repeated.
                let options = owner_changed_options(&mimes);

                let sessions = sessions_clone.lock().clone();
                for session in sessions {
                    let per_session: HashMap<&str, &Value<'_>> =
                        options.iter().map(|(k, v)| (*k, v)).collect();
                    let _ = Self::selection_owner_changed(&emitter, &session, per_session).await;
                }
            }
        });

        Self {
            active_sessions,
            pending_transfers,
            connection,
            proxy,
            session_manager,
        }
    }
}

impl ClipboardPortal {
    /// Reject operations on a session that never called `RequestClipboard`.
    ///
    /// GNOME resolves the session first and answers
    /// `org.freedesktop.portal.Error.NotFound`; KDE answers
    /// `QDBusError::InvalidArgs` ("not a clipboard enabled session"). Without
    /// this check the session argument was accepted and discarded, so any
    /// sandboxed app holding a clipboard session could drive another session's
    /// transfers.
    fn require_clipboard_session(&self, session: &ObjectPath<'_>) -> fdo::Result<String> {
        let session = session.as_str();
        if self
            .active_sessions
            .lock()
            .iter()
            .any(|s| s.as_str() == session)
        {
            Ok(session.to_owned())
        } else {
            Err(fdo::Error::InvalidArgs(format!(
                "Session {session} has not requested clipboard access"
            )))
        }
    }
}

#[interface(name = "org.freedesktop.impl.portal.Clipboard")]
impl ClipboardPortal {
    async fn request_clipboard(
        &self,
        #[zbus(header)] header: Header<'_>,
        session_handle: ObjectPath<'_>,
        _options: HashMap<&str, Value<'_>>,
    ) -> fdo::Result<()> {
        let sender = header
            .sender()
            .map(|s| String::from(s.as_str()))
            .ok_or_else(|| fdo::Error::Failed("Missing sender".into()))?;

        tracing::debug!("RequestClipboard called for session: {:?}", session_handle);
        let session_handle_owned = session_handle.into_owned();
        {
            let mut sessions = self.active_sessions.lock();
            if !sessions.contains(&session_handle_owned) {
                sessions.push(session_handle_owned.clone());
            }
        }

        let cancel_notify = Arc::new(Notify::new());
        if let Err(e) = self.session_manager.register(
            "clipboard", // app_id isn't directly available, but we can use "clipboard" or just skip rate limiting
            &sender,
            session_handle_owned.as_str(),
            cancel_notify.clone(),
        ) {
            tracing::warn!("Session limit exceeded for clipboard: {}", e);
            // Even if it fails, we continue, but we won't clean up automatically.
            //
            // Both the `active_sessions` entry pushed above and any serial
            // counter this session later creates are therefore retained until
            // the process exits. This is pre-existing for `active_sessions`, and
            // it is trivially reachable: the clipboard portal registers every
            // client under the shared pseudo-app-id `"clipboard"`, so any one
            // unprivileged app calling `RequestClipboard` ten times drives the
            // counter to the limit and every later call from *any* app takes this
            // branch, permanently.
            //
            // Not fixed here because closing it means changing session-limit
            // semantics (reject the request, or give the clipboard portal a
            // per-sender budget) rather than optimising a data structure. The
            // serial counter is retained along with it, so `SERIALS` grows
            // without bound on this path too.
        } else {
            let active_sessions_clone = self.active_sessions.clone();
            let session_handle_clone = session_handle_owned.clone();
            let session_manager_clone = self.session_manager.clone();
            tokio::spawn(async move {
                cancel_notify.notified().await;
                tracing::debug!(
                    "App {} disconnected, cleaning up clipboard session {:?}",
                    sender,
                    session_handle_clone
                );
                active_sessions_clone
                    .lock()
                    .retain(|s| s != &session_handle_clone);
                session_manager_clone.unregister(
                    "clipboard",
                    &sender,
                    session_handle_clone.as_str(),
                );
                if forget_session(session_handle_clone.as_str()) {
                    tracing::debug!(
                        "Released clipboard serial counter for {:?}",
                        session_handle_clone
                    );
                }
            });
        }

        let conn_clone = self.connection.clone();
        let mimes = crate::gui::run_ui_task(
            &self.proxy,
            |tx, _, _| {
                let mimes = gtk_backend::current_formats().unwrap_or_default();
                let _ = tx.dispatch(Ok::<_, fdo::Error>(mimes));
            },
            || fdo::Error::Failed("UI task cancelled".into()),
        )
        .await
        .unwrap_or_default();

        if let Ok(emitter) = SignalEmitter::new(&conn_clone, crate::core::DBUS_PATH) {
            let options = owner_changed_options(&mimes);
            tracing::debug!(
                "Emitting SelectionOwnerChanged for {:?} with mimes: {:?}",
                session_handle_owned,
                options.get("mime_types")
            );
            let per_session: HashMap<&str, &Value<'_>> =
                options.iter().map(|(k, v)| (*k, v)).collect();
            if let Err(e) =
                Self::selection_owner_changed(&emitter, &session_handle_owned, per_session).await
            {
                tracing::error!("Failed to emit SelectionOwnerChanged: {}", e);
            } else {
                tracing::debug!("Successfully emitted SelectionOwnerChanged");
            }
        } else {
            tracing::error!("Failed to create SignalEmitter");
        }
        Ok(())
    }

    async fn set_selection(
        &self,
        session_handle: ObjectPath<'_>,
        options: HashMap<&str, Value<'_>>,
    ) -> fdo::Result<()> {
        // Same session check as the three read-side methods above, and for the
        // same reason: without it, any app that holds *any* clipboard session
        // could name an arbitrary foreign `session_handle` and take over the
        // host clipboard on another app's behalf, emitting `SelectionTransfer`
        // under a session it does not own. A session handle is not a capability,
        // so accepting an un-registered one grants nothing that the caller did
        // not already have. See [`Self::require_clipboard_session`].
        self.require_clipboard_session(&session_handle)?;
        tracing::debug!("SetSelection called for session: {:?}", session_handle);
        let mimes = parse_mime_types(&options);

        let (tx, rx) = channel();
        let _ = self.proxy.sender.send(Box::new(move || {
            let res = gtk_backend::claim_selection(mimes);
            let _ = tx.send(res);
        }));

        let mut request_rx = rx
            .await
            .map_err(|_| fdo::Error::Failed("UI thread dropped channel".into()))?
            .map_err(|e| fdo::Error::Failed(format!("Failed to claim selection: {}", e)))?;

        let pending_transfers_clone = self.pending_transfers.clone();
        let conn_clone = self.connection.clone();
        let session_handle_owned = session_handle.into_owned();

        tokio::spawn(async move {
            // This task handles the host requesting data *from* the sandbox.
            // It dies when `request_rx` is dropped, which happens when the host copies
            // something else and our ContentProvider is destroyed.
            while let Some((mime, fd_sender)) = request_rx.recv().await {
                let Ok(emitter) = SignalEmitter::new(&conn_clone, crate::core::DBUS_PATH) else {
                    return;
                };

                // Serials are a per-session namespace, so each session gets its
                // own counter. A process-wide counter would let one app's
                // serial be predicted by another app.
                let serial = next_serial(&session_handle_owned);

                let key = (session_handle_owned.to_string(), serial);
                pending_transfers_clone
                    .lock()
                    .insert(key.clone(), TransferRequest { fd_sender });

                if let Err(e) =
                    Self::selection_transfer(&emitter, &session_handle_owned, &mime, serial).await
                {
                    tracing::error!("Failed to emit SelectionTransfer: {}", e);
                    pending_transfers_clone.lock().remove(&key);
                } else {
                    let pending = pending_transfers_clone.clone();
                    tokio::spawn(async move {
                        tokio::time::sleep(Duration::from_secs(10)).await;
                        if pending.lock().remove(&key).is_some() {
                            tracing::warn!("Clipboard transfer request {serial} timed out");
                        }
                    });
                }
            }

            // NOTE: the counter is deliberately *not* released here. `SetSelection`
            // is re-callable, and `set_content` drops the previous provider, so
            // this loop can end while a newer task for the same session handle is
            // still minting serials. Releasing here would restart that namespace
            // at 1 and overwrite a still-pending `(session, 1)` transfer. The
            // counter is released on session teardown instead.
        });

        Ok(())
    }

    async fn selection_write(
        &self,
        session_handle: ObjectPath<'_>,
        serial: u32,
    ) -> fdo::Result<Fd<'_>> {
        let session = self.require_clipboard_session(&session_handle)?;
        tracing::debug!("SelectionWrite called for session: {session} serial: {serial}");
        let transfer = self
            .pending_transfers
            .lock()
            .remove(&(session, serial))
            .ok_or_else(|| fdo::Error::InvalidArgs(format!("Invalid serial {serial}")))?;

        let (read_fd, write_fd) = rustix::pipe::pipe()
            .map_err(|e| fdo::Error::Failed(format!("Failed to create pipe: {}", e)))?;

        // Send the read end to the backend provider
        if transfer.fd_sender.send(read_fd).is_err() {
            return Err(fdo::Error::Failed("Backend is no longer listening".into()));
        }

        // Return the write end to the DBus caller so they can stream data directly
        // to the GTK backend via the pipe.
        Ok(Fd::from(write_fd))
    }

    async fn selection_write_done(
        &self,
        session_handle: ObjectPath<'_>,
        serial: u32,
        success: bool,
    ) -> fdo::Result<()> {
        let session = self.require_clipboard_session(&session_handle)?;
        tracing::debug!(
            "SelectionWriteDone called for session: {session} serial: {serial} success: {success}"
        );
        self.pending_transfers.lock().remove(&(session, serial));
        Ok(())
    }

    async fn selection_read(
        &self,
        session_handle: ObjectPath<'_>,
        mime_type: String,
    ) -> fdo::Result<Fd<'_>> {
        let session = self.require_clipboard_session(&session_handle)?;
        tracing::debug!("SelectionRead called for session: {session} mime_type: {mime_type}");

        let (read_fd, write_fd) = rustix::pipe::pipe()
            .map_err(|e| fdo::Error::Failed(format!("Failed to create pipe: {}", e)))?;

        let _ = self.proxy.sender.send(Box::new(move || {
            if let Err(e) = gtk_backend::read_selection(mime_type.into_boxed_str(), write_fd) {
                tracing::error!("Failed to read selection: {}", e);
            }
        }));

        Ok(Fd::from(read_fd))
    }

    #[zbus(signal)]
    async fn selection_owner_changed(
        ctxt: &SignalEmitter<'_>,
        session_handle: &ObjectPath<'_>,
        options: HashMap<&str, &Value<'_>>,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn selection_transfer(
        ctxt: &SignalEmitter<'_>,
        session_handle: &ObjectPath<'_>,
        mime_type: &str,
        serial: u32,
    ) -> zbus::Result<()>;

    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        2
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*, crate::core::session_manager::SessionManager, gtk4::glib::MainContext,
        tokio::sync::mpsc::unbounded_channel,
    };

    fn dummy_proxy() -> UiProxy {
        let (sender, _receiver) = unbounded_channel();
        UiProxy {
            context: MainContext::default(),
            sender,
        }
    }

    #[tokio::test]
    async fn test_clipboard_version() -> Result<(), Box<dyn std::error::Error>> {
        if std::env::var("RUN_DBUS_TESTS").is_err() {
            return Ok(());
        }
        let conn = Connection::session().await?;
        let sm = SessionManager::new(conn.clone(), 10);
        let portal = ClipboardPortal::new(conn, dummy_proxy(), sm);
        assert_eq!(portal.version(), 2);
        Ok(())
    }

    #[tokio::test]
    async fn test_selection_write_invalid_serial() -> Result<(), Box<dyn std::error::Error>> {
        if std::env::var("RUN_DBUS_TESTS").is_err() {
            return Ok(());
        }
        let conn = Connection::session().await?;
        let sm = SessionManager::new(conn.clone(), 10);
        let portal = ClipboardPortal::new(conn, dummy_proxy(), sm);

        let path = ObjectPath::try_from("/org/freedesktop/portal/desktop/session/1/1").unwrap();
        let res = portal.selection_write(path.clone(), 9999).await;

        assert!(res.is_err());
        assert!(matches!(res.unwrap_err(), fdo::Error::InvalidArgs(_)));
        Ok(())
    }

    /// Builds a portal with `sessions` pre-registered, without going through the
    /// bus or GTK.
    fn portal_with_sessions(conn: &Connection, sessions: &[&'static str]) -> ClipboardPortal {
        let portal = ClipboardPortal::new(
            conn.clone(),
            dummy_proxy(),
            SessionManager::new(conn.clone(), 10),
        );
        portal
            .active_sessions
            .lock()
            .extend(sessions.iter().map(|s| ObjectPath::try_from(*s).unwrap()));
        portal
    }

    /// The four session-scoped methods must all reject a handle that never went
    /// through `RequestClipboard`.
    ///
    /// Regression: `SetSelection` was the only one that did not check. The other
    /// three already rejected a foreign handle, but `SetSelection` accepted any
    /// `ObjectPath` at all, so an app holding one clipboard session could name
    /// another app's session and take the host clipboard on its behalf, getting
    /// `SelectionTransfer` signals emitted under a session it does not own. The
    /// portal contract is explicit: "May only be called if clipboard access was
    /// given after starting the session."
    #[tokio::test]
    async fn an_unregistered_session_is_refused_by_every_session_method(
    ) -> Result<(), Box<dyn std::error::Error>> {
        if std::env::var("RUN_DBUS_TESTS").is_err() {
            return Ok(());
        }
        let conn = Connection::session().await?;
        let mine = "/org/freedesktop/portal/desktop/session/1/1";
        let portal = portal_with_sessions(&conn, &[mine]);

        let foreign = ObjectPath::try_from("/org/freedesktop/portal/desktop/session/9/9")?;

        let set = portal
            .set_selection(foreign.clone(), HashMap::new())
            .await;
        assert!(
            matches!(set, Err(fdo::Error::InvalidArgs(_))),
            "SetSelection accepted a session that never requested clipboard access: {set:?}"
        );

        let write = portal.selection_write(foreign.clone(), 1).await;
        assert!(matches!(write, Err(fdo::Error::InvalidArgs(_))));

        let read = portal
            .selection_read(foreign.clone(), "text/plain".into())
            .await;
        assert!(matches!(read, Err(fdo::Error::InvalidArgs(_))));

        let done = portal.selection_write_done(foreign, 1, true).await;
        assert!(matches!(done, Err(fdo::Error::InvalidArgs(_))));

        Ok(())
    }

    /// The check must not break the ordinary flow: a registered handle still
    /// passes.
    #[tokio::test]
    async fn a_registered_session_is_accepted() -> Result<(), Box<dyn std::error::Error>> {
        if std::env::var("RUN_DBUS_TESTS").is_err() {
            return Ok(());
        }
        let conn = Connection::session().await?;
        const MINE: &str = "/org/freedesktop/portal/desktop/session/1/1";
        let mine = ObjectPath::try_from(MINE)?;
        let portal = portal_with_sessions(&conn, &[MINE]);

        assert!(portal.require_clipboard_session(&mine).is_ok());
        Ok(())
    }
}

#[cfg(test)]
mod serial_tests {
    use super::*;

    /// `SERIALS` is a process-wide `static` shared by every test in this binary,
    /// and `cargo test` runs tests on parallel threads. Serialising these tests
    /// against each other makes the absolute-count assertions below sound; the
    /// unique session names keep them independent of the surrounding suite.
    static SERIAL_TEST_LOCK: Mutex<()> = Mutex::new(());

    /// Holds the serial-test lock for its lifetime and releases any serial
    /// counters a test created, so a failing assertion cannot leak state into
    /// the next test.
    struct SerialTestGuard {
        _lock: parking_lot::MutexGuard<'static, ()>,
        sessions: Vec<&'static str>,
    }

    impl SerialTestGuard {
        fn new(sessions: &[&'static str]) -> Self {
            let lock = SERIAL_TEST_LOCK.lock();
            Self {
                _lock: lock,
                sessions: sessions.to_vec(),
            }
        }
    }

    impl Drop for SerialTestGuard {
        fn drop(&mut self) {
            for session in &self.sessions {
                forget_session(session);
            }
        }
    }

    #[test]
    fn serials_are_per_session_namespaces() {
        let _guard = SerialTestGuard::new(&[
            "/test/clipboard/serials/ns-a",
            "/test/clipboard/serials/ns-b",
        ]);
        assert_eq!(next_serial("/test/clipboard/serials/ns-a"), 1);
        assert_eq!(
            next_serial("/test/clipboard/serials/ns-b"),
            1,
            "counters must not be shared across sessions",
        );
    }

    #[test]
    fn serials_increment_within_a_session() {
        let _guard = SerialTestGuard::new(&["/test/clipboard/serials/increment"]);
        let s = "/test/clipboard/serials/increment";
        assert_eq!(next_serial(s), 1);
        assert_eq!(next_serial(s), 2);
        assert_eq!(next_serial(s), 3);
    }

    #[test]
    fn serial_never_returns_the_reserved_zero() {
        let _guard = SerialTestGuard::new(&["/test/clipboard/serials/zero"]);
        let s = "/test/clipboard/serials/zero";
        for _ in 0..8 {
            assert_ne!(next_serial(s), 0, "0 is reserved by the contract");
        }
    }

    #[test]
    fn counter_is_created_lazily_only_when_requested() {
        let _guard = SerialTestGuard::new(&["/test/clipboard/serials/lazy"]);
        let s = "/test/clipboard/serials/lazy";
        // Deltas rather than absolutes: `SERIALS` is process-wide, so an
        // absolute count is only correct while nothing else in the binary is
        // using it.
        let before = tracked_serial_count();
        next_serial(s);
        assert_eq!(
            tracked_serial_count(),
            before + 1,
            "using a serial creates exactly one counter",
        );
    }

    #[test]
    fn repeated_use_does_not_grow_the_map() {
        let _guard = SerialTestGuard::new(&["/test/clipboard/serials/stable"]);
        let s = "/test/clipboard/serials/stable";
        next_serial(s);
        let after_first = tracked_serial_count();
        for _ in 0..100 {
            next_serial(s);
        }
        assert_eq!(
            tracked_serial_count(),
            after_first,
            "repeated SetSelection on one session must reuse one counter",
        );
    }

    #[test]
    fn forget_session_releases_the_counter() {
        let _guard = SerialTestGuard::new(&["/test/clipboard/serials/forget"]);
        let s = "/test/clipboard/serials/forget";
        next_serial(s);
        let held = tracked_serial_count();
        assert!(forget_session(s), "removal must report that it removed one");
        assert_eq!(
            tracked_serial_count(),
            held - 1,
            "teardown must not leave the counter behind",
        );
        assert!(!forget_session(s), "removal is idempotent");
    }

    /// The regression this guards: `next_serial` used to build an owned key for
    /// `HashMap::entry`, so every clipboard request allocated a session `Arc`
    /// even when the session already had a counter.
    #[test]
    fn a_warm_counter_lookup_does_not_allocate() {
        let _guard = SerialTestGuard::new(&["/test/clipboard/serials/warm"]);
        let s = "/test/clipboard/serials/warm";
        assert_eq!(next_serial(s), 1);

        let scope = crate::alloc_probe::AllocScope::start();
        let serial = next_serial(s);
        let snap = scope.finish();

        assert_eq!(serial, 2, "the counter must keep advancing");
        assert_eq!(
            snap.count, 0,
            "an existing counter must be found by borrow, not rebuilt, got {snap:?}",
        );
    }

    #[test]
    fn forgetting_one_session_leaves_others_alone() {
        let _guard = SerialTestGuard::new(&[
            "/test/clipboard/serials/keep",
            "/test/clipboard/serials/drop",
        ]);
        next_serial("/test/clipboard/serials/keep");
        next_serial("/test/clipboard/serials/drop");
        forget_session("/test/clipboard/serials/drop");
        assert_eq!(
            tracked_serial_count(),
            1,
            "only the forgotten session's counter is released",
        );
        // A surviving session keeps counting where it left off, so releasing
        // another session's counter cannot restart it.
        assert_eq!(next_serial("/test/clipboard/serials/keep"), 2);
    }

    /// Documents the `forget_session` precondition: a counter is released only
    /// once the session is finished, so a live session's serials stay unique.
    /// (A still-live `SetSelection` task after teardown would restart the
    /// namespace, but its serials can never be claimed: `selection_write`
    /// requires the session, which teardown already removed.)
    #[test]
    fn a_forgotten_session_starts_from_scratch_only_once() {
        let _guard = SerialTestGuard::new(&["/test/clipboard/serials/restart"]);
        let s = "/test/clipboard/serials/restart";
        assert_eq!(next_serial(s), 1);
        assert_eq!(next_serial(s), 2);
        assert!(forget_session(s));
        // Minting again after the namespace was released starts a fresh counter.
        assert_eq!(next_serial(s), 1);
        assert!(
            forget_session(s),
            "the resurrected counter must be releasable too"
        );
    }
}

#[cfg(test)]
mod payload_tests {
    use super::*;

    fn mimes(values: &[&str]) -> gtk_backend::MimeList {
        values.iter().map(|v| (*v).into()).collect()
    }

    /// The `SelectionOwnerChanged` payload is what every clipboard client reads,
    /// and it is now built in one shared place rather than duplicated per caller.
    #[test]
    fn owner_changed_payload_shape() {
        let list = mimes(&["text/plain", "image/png"]);
        let options = owner_changed_options(&list);

        assert_eq!(options.len(), 2, "exactly the two contract keys");
        assert_eq!(options.get("session_is_owner"), Some(&Value::from(false)));

        let Some(Value::Array(array)) = options.get("mime_types") else {
            panic!("mime_types must be an array");
        };
        assert_eq!(array.len(), 2);
        assert_eq!(
            array.get(0).ok().flatten(),
            Some(&Value::from("text/plain"))
        );
        assert_eq!(array.get(1).ok().flatten(), Some(&Value::from("image/png")));
    }

    #[test]
    fn mime_order_is_preserved() {
        let list = mimes(&["z/last", "a/first"]);
        let options = owner_changed_options(&list);
        let Some(Value::Array(array)) = options.get("mime_types") else {
            panic!("mime_types must be an array");
        };
        assert_eq!(array.get(0).ok().flatten(), Some(&Value::from("z/last")));
        assert_eq!(array.get(1).ok().flatten(), Some(&Value::from("a/first")));
    }

    #[test]
    fn empty_mime_list_still_produces_an_array() {
        let list = gtk_backend::MimeList::new();
        let options = owner_changed_options(&list);
        assert!(matches!(options.get("mime_types"), Some(Value::Array(a)) if a.is_empty()));
    }

    #[test]
    fn mime_types_round_trip_through_the_wire_value() {
        let original = mimes(&["text/plain", "text/html", "image/png"]);
        let options = owner_changed_options(&original);
        assert_eq!(parse_mime_types(&options), original);
    }

    #[test]
    fn parsing_a_missing_or_wrongly_typed_key_yields_an_empty_list() {
        let empty: HashMap<&str, Value<'_>> = HashMap::new();
        assert!(parse_mime_types(&empty).is_empty());

        let wrong_type: HashMap<&str, Value<'_>> =
            HashMap::from([("mime_types", Value::from("text/plain"))]);
        assert!(parse_mime_types(&wrong_type).is_empty());
    }

    #[test]
    fn parsing_skips_non_string_elements() {
        let mixed = Value::from(vec![
            Value::from("text/plain"),
            Value::from(42u32),
            Value::from("image/png"),
        ]);
        let options: HashMap<&str, Value<'_>> = HashMap::from([("mime_types", mixed)]);
        let parsed = parse_mime_types(&options);
        assert_eq!(parsed, mimes(&["text/plain", "image/png"]));
    }
}
