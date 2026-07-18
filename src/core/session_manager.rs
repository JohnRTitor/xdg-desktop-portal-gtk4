use {
    async_channel::Sender,
    futures_util::stream::StreamExt,
    std::{
        collections::HashMap,
        sync::{Arc, Mutex},
    },
    zbus::{Connection, fdo::DBusProxy},
};

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("Too many sessions for app {app_id}")]
    LimitExceeded { app_id: String },
}

#[derive(Default)]
pub struct SessionManagerState {
    // Maps sender -> list of (object_path, app_id, cancel_sender)
    sender_objects: HashMap<String, Vec<(String, String, Sender<()>)>>,

    // Maps app_id -> count of active sessions
    app_sessions: HashMap<String, usize>,
}

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

    /// Registers a session or request with the session manager.
    pub fn register(
        &self,
        app_id: &str,
        sender: &str,
        object_path: &str,
        cancel: Sender<()>,
    ) -> Result<(), SessionError> {
        let mut state = self.state.lock().unwrap();

        let current_count = state.app_sessions.get(app_id).copied().unwrap_or(0);
        if current_count >= self.max_sessions_per_app {
            return Err(SessionError::LimitExceeded {
                app_id: app_id.to_string(),
            });
        }

        if let Some(count) = state.app_sessions.get_mut(app_id) {
            *count += 1;
        } else {
            state.app_sessions.insert(app_id.to_string(), 1);
        }

        let sender_list = if state.sender_objects.contains_key(sender) {
            state.sender_objects.get_mut(sender).unwrap()
        } else {
            state.sender_objects.entry(sender.to_string()).or_default()
        };
        sender_list.push((object_path.to_string(), app_id.to_string(), cancel));

        Ok(())
    }

    /// Unregisters a session or request.
    pub fn unregister(&self, app_id: &str, sender: &str, object_path: &str) {
        let mut state = self.state.lock().unwrap();

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
    pub async fn run(&self) -> zbus::Result<()> {
        let proxy = DBusProxy::new(&self.conn).await?;
        let mut name_owner_changed = proxy.receive_name_owner_changed().await?;

        while let Some(signal) = name_owner_changed.next().await {
            let args = signal.args()?;
            // If new_owner is empty, it means the name was lost (disconnected)
            if args
                .new_owner()
                .as_ref()
                .map_or(true, |n| n.as_str().is_empty())
            {
                let name = args.name().as_str();

                let objects_to_close = {
                    let mut state = self.state.lock().unwrap();
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
                    log::info!("Client {} disconnected, cancelling {}", name, path);
                    let _ = cancel.send(()).await;
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

        let (send, _) = async_channel::bounded(1);

        assert!(
            manager
                .register("app1", "sender1", "/path1", send.clone())
                .is_ok()
        );
        assert!(
            manager
                .register("app1", "sender1", "/path2", send.clone())
                .is_ok()
        );

        // Third should fail due to limit
        let res = manager.register("app1", "sender2", "/path3", send.clone());
        assert!(matches!(res, Err(SessionError::LimitExceeded { .. })));

        // Unregister one
        manager.unregister("app1", "sender1", "/path1");

        // Now registering should succeed
        assert!(
            manager
                .register("app1", "sender2", "/path3", send.clone())
                .is_ok()
        );

        // Unregister remaining
        manager.unregister("app1", "sender1", "/path2");
        manager.unregister("app1", "sender2", "/path3");

        let state = manager.state.lock().unwrap();
        assert!(state.app_sessions.is_empty());
        assert!(state.sender_objects.is_empty());
    }
}
