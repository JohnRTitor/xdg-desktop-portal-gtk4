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
    std::{
        collections::HashMap,
        sync::Arc,
    },
    parking_lot::Mutex,
    tokio::sync::Notify,
    zbus::{Connection, fdo::DBusProxy},
};

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("Too many sessions for app {app_id}")]
    LimitExceeded { app_id: String },
}

type CancellableSender = Arc<Notify>;

#[derive(Default)]
pub(crate) struct SessionManagerState {
    /// Maps a D-Bus sender name (e.g., ":1.42") to a list of its active requests.
    ///
    /// Each request is represented by its object path, the app ID, and a oneshot
    /// cancellation sender. This allows us to instantly notify the specific request
    /// task to abort when the sender disconnects.
    sender_objects: HashMap<String, Vec<(String, String, CancellableSender)>>,

    // Maps an application ID (e.g., "org.gnome.TextEditor") to the number of active sessions.
    // Used to enforce rate-limiting / spam prevention (max_sessions_per_app).
    app_sessions: HashMap<String, usize>,
}

/// Tracks active portal sessions and cancels them if the calling application exits.
///
/// # Synchronization Strategy
///
/// We use a standard `std::sync::Mutex` rather than `tokio::sync::Mutex` because
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
        let mut state = self
            .state
            .lock();

        let count = state.app_sessions.get_mut(app_id);
        if let Some(count_ref) = count {
            if *count_ref >= self.max_sessions_per_app {
                return Err(SessionError::LimitExceeded {
                    app_id: app_id.to_string(),
                });
            }
            *count_ref += 1;
        } else {
            state.app_sessions.insert(app_id.to_string(), 1);
        }

        let sender_list = state.sender_objects.get_mut(sender);
        if let Some(list) = sender_list {
            list.push((object_path.to_string(), app_id.to_string(), cancel));
        } else {
            state.sender_objects.insert(
                sender.to_string(),
                vec![(object_path.to_string(), app_id.to_string(), cancel)],
            );
        }

        Ok(())
    }

    /// Unregisters a session or request.
    ///
    /// This should be called when a request naturally completes (either success, cancellation, or error)
    /// so that we don't leak cancellation senders and the application's session count decrements.
    pub fn unregister(&self, app_id: &str, sender: &str, object_path: &str) {
        let mut state = self
            .state
            .lock();

        if let Some(count) = state.app_sessions.get_mut(app_id) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                state.app_sessions.remove(app_id);
            }
        }

        if let Some(objects) = state.sender_objects.get_mut(sender) {
            objects.retain(|(p, _, _)| p != object_path);
            if objects.is_empty() {
                state.sender_objects.remove(sender);
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
                    let mut state = self
                        .state
                        .lock();
                    let closed = state.sender_objects.remove(name).unwrap_or_default();

                    for (_, app_id, _) in &closed {
                        if let Some(count) = state.app_sessions.get_mut(app_id) {
                            *count = count.saturating_sub(1);
                            if *count == 0 {
                                state.app_sessions.remove(app_id);
                            }
                        }
                    }
                    closed
                };

                for (path, _, cancel) in objects_to_close {
                    tracing::info!("Client {} disconnected, cancelling {}", name, path);
                    cancel.notify_one();
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use {super::*, zbus::Connection};

    #[tokio::test]
    async fn test_session_manager_register_unregister() {
        let conn_result = Connection::session().await;
        if conn_result.is_err() {
            println!("Skipping dbus test because connection failed");
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

        let state = manager
            .state
            .lock();
        assert!(state.app_sessions.is_empty());
        assert!(state.sender_objects.is_empty());
    }
}
