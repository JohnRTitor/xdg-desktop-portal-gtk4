//! Session management and lifecycle tracking for active portal requests.
//!
//! When a sandboxed application requests a portal action (e.g., opening a file chooser),
//! it holds a D-Bus connection. If that application crashes or exits unexpectedly,
//! the portal must clean up any active dialogs or resources associated with that request.
//!
//! The `SessionManager` achieves this by monitoring the `org.freedesktop.DBus.NameOwnerChanged`
//! signal. It maps D-Bus sender names (e.g., `:1.42`) to active request cancellation channels.
//! When a sender drops off the bus, the session manager automatically triggers cancellation
//! for all of its active portal requests.

use {
    futures_util::stream::StreamExt,
    parking_lot::Mutex,
    std::{collections::HashMap, sync::Arc},
    tokio::sync::Notify,
    zbus::{Connection, fdo::DBusProxy},
};

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("Too many sessions for app {app_id}")]
    LimitExceeded { app_id: String },
}

type CancellableSender = Arc<Notify>;

/// A request tracked against its app, kept alongside the sender's request list.
struct TrackedRequest {
    object_path: String,
    app_id: Arc<str>,
    cancel: CancellableSender,
}

#[derive(Default)]
pub(crate) struct SessionManagerState {
    /// Maps a D-Bus sender name (e.g., ":1.42") to a list of its active requests.
    ///
    /// Each request records its object path, the app ID, and a cancellation
    /// sender. This allows us to instantly notify the specific request task to
    /// abort when the sender disconnects.
    sender_objects: HashMap<String, Vec<TrackedRequest>>,

    // Maps an application ID (e.g., "org.gnome.TextEditor") to the number of active sessions.
    // Used to enforce rate-limiting / spam prevention (max_sessions_per_app).
    app_sessions: HashMap<Arc<str>, usize>,
}

/// Tracks active portal sessions and cancels them if the calling application exits.
///
/// # Synchronization Strategy
///
/// We use a `parking_lot::Mutex` rather than `tokio::sync::Mutex` or `std::sync::Mutex` because
/// the critical sections (register/unregister/cleanup) are extremely short (just
/// HashMap operations) and never cross `.await` points. This avoids the overhead
/// and potential deadlocks of asynchronous locking for simple state.
#[derive(Clone)]
pub struct SessionManager {
    state: Arc<Mutex<SessionManagerState>>,
    conn: Connection,
    max_sessions_per_app: usize,
}

impl SessionManager {
    pub fn new(conn: Connection, max_sessions_per_app: usize) -> Self {
        Self {
            state: Arc::new(Mutex::new(SessionManagerState::default())),
            conn,
            max_sessions_per_app,
        }
    }

    /// Returns the underlying D-Bus connection.
    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    /// Registers a session or request with the session manager.
    ///
    /// This should be called when a new portal request starts.
    /// If the application has exceeded its concurrent session limit, this returns `SessionError::LimitExceeded`.
    pub fn register(
        &self,
        app_id: &str,
        sender: &str,
        object_path: &str,
        cancel: CancellableSender,
    ) -> Result<(), SessionError> {
        let mut state = self.state.lock();
        register_tracked(
            &mut state,
            self.max_sessions_per_app,
            app_id,
            sender,
            object_path,
            cancel,
        )
    }

    fn push_tracked(
        state: &mut SessionManagerState,
        sender: &str,
        object_path: &str,
        app_id: Arc<str>,
        cancel: CancellableSender,
    ) {
        let entry = TrackedRequest {
            object_path: object_path.into(),
            app_id,
            cancel,
        };
        match state.sender_objects.get_mut(sender) {
            Some(list) => list.push(entry),
            None => {
                state.sender_objects.insert(sender.into(), vec![entry]);
            }
        }
    }

    /// Unregisters a session or request.
    ///
    /// This should be called when a request naturally completes (either success, cancellation, or error)
    /// so that we don't leak cancellation senders and the application's session count decrements.
    ///
    /// Releasing the per-app budget slot is conditional on actually still
    /// holding the registration. The `NameOwnerChanged` sweep in [`Self::run`]
    /// removes a departed sender's entries *and* decrements the counter once
    /// per entry; `run_request` then calls this for each of those same
    /// requests as their futures unwind. Unconditionally decrementing here
    /// therefore counted every departed request twice, which drove the app's
    /// count to zero and removed the entry entirely -- freeing the app's
    /// budget while its still-live requests on other senders were running.
    pub fn unregister(&self, app_id: &str, sender: &str, object_path: &str) {
        let mut state = self.state.lock();

        // Fold the emptiness test into the borrow already held, so `sender` is
        // hashed once here rather than twice.
        let (held, sender_is_empty) = match state.sender_objects.get_mut(sender) {
            Some(objects) => {
                let before = objects.len();
                objects.retain(|req| req.object_path != object_path);
                (objects.len() != before, objects.is_empty())
            }
            None => (false, false),
        };

        if sender_is_empty {
            state.sender_objects.remove(sender);
        }

        if !held {
            return;
        }

        if let Some(count) = state.app_sessions.get_mut(app_id) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                state.app_sessions.remove(app_id);
            }
        }
    }

    /// Runs the background task that listens for NameOwnerChanged.
    ///
    /// This should be spawned on a background Tokio task and run indefinitely.
    /// It intercepts D-Bus disconnection events and drops any state tied to dead clients.
    pub async fn run(&self) -> zbus::Result<()> {
        let proxy = DBusProxy::new(&self.conn).await?;
        let mut name_owner_changed = proxy.receive_name_owner_changed().await?;

        while let Some(signal) = name_owner_changed.next().await {
            let args = signal.args()?;
            // If new_owner is empty, it means the name was lost (disconnected)
            if args
                .new_owner()
                .as_ref()
                .is_none_or(|n| n.as_str().is_empty())
            {
                let name = args.name().as_str();

                let objects_to_close = {
                    let mut state = self.state.lock();
                    sweep_sender(&mut state, name)
                };

                for req in objects_to_close {
                    tracing::info!(
                        "Client {} disconnected, cancelling {}",
                        name,
                        req.object_path
                    );
                    req.cancel.notify_one();
                }
            }
        }
        Ok(())
    }
}

/// The body of [`SessionManager::register`], without the lock.
///
/// Split out so the budget and key-sharing rules can be tested without a D-Bus
/// connection.
fn register_tracked(
    state: &mut SessionManagerState,
    max_sessions_per_app: usize,
    app_id: &str,
    sender: &str,
    object_path: &str,
    cancel: CancellableSender,
) -> Result<(), SessionError> {
    // `Arc<str>: Borrow<str>`, so `get_key_value` finds an app that is already
    // counted without allocating. Only a genuinely new app pays for its key;
    // everything else is a refcount bump on the existing one.
    let shared_app_id: Arc<str> = match state.app_sessions.get_key_value(app_id) {
        Some((key, count)) => {
            // Checked against the pre-increment count, and a refusal returns
            // before anything is tracked, so a rejected request never leaves an
            // entry that the disconnect sweep would later decrement.
            if *count >= max_sessions_per_app {
                return Err(SessionError::LimitExceeded {
                    app_id: app_id.into(),
                });
            }
            key.clone()
        }
        None => Arc::from(app_id),
    };

    // The borrow from `get_key_value` ended with the match above, so this mutable
    // access is fine.
    match state.app_sessions.get_mut(shared_app_id.as_ref()) {
        Some(count) => *count += 1,
        None => {
            state.app_sessions.insert(shared_app_id.clone(), 1);
        }
    }

    SessionManager::push_tracked(state, sender, object_path, shared_app_id, cancel);
    Ok(())
}

/// Drops every request belonging to a departed sender and releases the per-app
/// budget slots they held.
///
/// Returns the removed requests so the caller can notify their cancellation
/// senders. Factored out of [`SessionManager::run`] so it can be tested without a
/// D-Bus connection: this is where the `app_sessions` key type matters, and a
/// lookup that stopped matching would silently leak budget slots until the app
/// was permanently rate-limited.
fn sweep_sender(state: &mut SessionManagerState, sender: &str) -> Vec<TrackedRequest> {
    let closed = state.sender_objects.remove(sender).unwrap_or_default();
    for req in &closed {
        if let Some(count) = state.app_sessions.get_mut(req.app_id.as_ref()) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                state.app_sessions.remove(req.app_id.as_ref());
            }
        }
    }
    closed
}

#[cfg(test)]
impl SessionManagerState {
    /// Test helper: registers one tracked request and counts it against
    /// `app_id`, bypassing the budget check.
    fn push_for_test(&mut self, sender: &str, object_path: &str, app_id: Arc<str>) {
        *self.app_sessions.entry(app_id.clone()).or_insert(0) += 1;
        SessionManager::push_tracked(self, sender, object_path, app_id, Arc::new(Notify::new()));
    }
}

#[cfg(test)]
mod tests {
    use {super::*, zbus::Connection};

    #[tokio::test]
    async fn test_session_manager_register_unregister() {
        // NOTE: this test returns early without asserting anything when there is
        // no session bus, so it is green but vacuous outside `dbus-run-session`.
        // `sweep_tests` above covers the counter bookkeeping without a bus.
        let conn_result = Connection::session().await;
        if conn_result.is_err() {
            println!("SKIPPED test_session_manager_register_unregister: no session bus");
            return;
        }
        let conn = conn_result.unwrap();
        let manager = SessionManager::new(conn, 2);

        let notify1 = Arc::new(Notify::new());
        let notify2 = Arc::new(Notify::new());
        let notify3 = Arc::new(Notify::new());
        let notify4 = Arc::new(Notify::new());

        assert!(
            manager
                .register("app1", "sender1", "/path1", notify1)
                .is_ok()
        );
        assert!(
            manager
                .register("app1", "sender1", "/path2", notify2)
                .is_ok()
        );

        // Third should fail due to limit
        let res = manager.register("app1", "sender2", "/path3", notify3);
        assert!(matches!(res, Err(SessionError::LimitExceeded { .. })));

        // Unregister one
        manager.unregister("app1", "sender1", "/path1");

        // Now registering should succeed
        assert!(
            manager
                .register("app1", "sender2", "/path3", notify4)
                .is_ok()
        );

        // Unregister remaining
        manager.unregister("app1", "sender1", "/path2");
        manager.unregister("app1", "sender2", "/path3");

        let state = manager.state.lock();
        assert!(state.app_sessions.is_empty());
        assert!(state.sender_objects.is_empty());
    }
}

#[cfg(test)]
mod sweep_tests {
    use super::*;

    fn state_with(entries: &[(&str, &str, &str)]) -> SessionManagerState {
        // (app_id, sender, object_path)
        let mut state = SessionManagerState::default();
        for (app_id, sender, object_path) in entries {
            state.push_for_test(sender, object_path, Arc::from(*app_id));
        }
        state
    }

    #[test]
    fn sweep_releases_the_budget_slot_per_request() {
        let mut state = state_with(&[("app", ":1", "/r1"), ("app", ":1", "/r2")]);
        let closed = sweep_sender(&mut state, ":1");
        assert_eq!(closed.len(), 2);
        assert!(
            state.sender_objects.is_empty(),
            "the sender's list must be gone"
        );
        assert_eq!(state.app_sessions.get("app"), None, "count reached zero");
    }

    #[test]
    fn sweep_leaves_the_entry_while_another_request_remains() {
        let mut state = state_with(&[("app", ":1", "/r1"), ("app", ":2", "/r2")]);
        sweep_sender(&mut state, ":1");
        assert_eq!(
            state.app_sessions.get("app"),
            Some(&1),
            "the surviving sender's request must keep its slot"
        );
        assert_eq!(state.sender_objects.len(), 1);
    }

    #[test]
    fn sweep_of_one_sender_does_not_touch_another_apps_budget() {
        let mut state = state_with(&[("a", ":1", "/r1"), ("b", ":1", "/r2")]);
        sweep_sender(&mut state, ":1");
        assert!(state.app_sessions.is_empty(), "both apps hit zero");
        assert!(state.sender_objects.is_empty());
    }

    #[test]
    fn sweep_of_an_unknown_sender_is_a_no_op() {
        let mut state = state_with(&[("app", ":1", "/r1")]);
        assert!(sweep_sender(&mut state, ":99").is_empty());
        assert_eq!(state.app_sessions.get("app"), Some(&1));
        assert_eq!(state.sender_objects.len(), 1);
    }

    #[test]
    fn sweep_repeatedly_does_not_double_decrement() {
        // The `NameOwnerChanged` sweep and `unregister` both act on the same
        // requests; each must decrement exactly once.
        let mut state = state_with(&[("app", ":1", "/r1")]);
        sweep_sender(&mut state, ":1");
        assert_eq!(state.app_sessions.get("app"), None);
        // A second sweep finds nothing and must not underflow a fresh app entry.
        state.app_sessions.insert(Arc::from("app"), 3);
        assert!(sweep_sender(&mut state, ":1").is_empty());
        assert_eq!(
            state.app_sessions.get("app"),
            Some(&3),
            "an unrelated count must be untouched"
        );
    }

    #[test]
    fn budget_never_reaches_zero_for_a_live_request() {
        // Guards the key-type coupling: if `req.app_id` ever stopped matching the
        // `app_sessions` key, the count would never be released and the app would
        // stay rate-limited after `max_sessions_per_app` disconnects.
        let mut state = state_with(&[("app", ":1", "/r1")]);
        for _ in 0..5 {
            sweep_sender(&mut state, ":1");
            state.push_for_test(":1", "/r1", Arc::from("app"));
        }
        assert_eq!(
            state.app_sessions.get("app"),
            Some(&1),
            "a live request must always retain exactly one slot"
        );
    }
}
