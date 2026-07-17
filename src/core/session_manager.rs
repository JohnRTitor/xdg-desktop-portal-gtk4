use {
    async_channel::Sender,
    futures_util::stream::StreamExt,
    std::{collections::HashMap, sync::{Arc, Mutex}},
    zbus::{fdo::DBusProxy, Connection},
};

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("Too many sessions for app {app_id}")]
    LimitExceeded { app_id: String },
}

#[derive(Default)]
pub struct SessionManagerState {
    // Maps sender -> list of (object_path, cancel_sender)
    sender_objects: HashMap<String, Vec<(String, Sender<()>)>>,

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

        let count = state.app_sessions.entry(app_id.to_string()).or_insert(0);
        if *count >= self.max_sessions_per_app {
            return Err(SessionError::LimitExceeded {
                app_id: app_id.to_string(),
            });
        }

        *count += 1;
        state
            .sender_objects
            .entry(sender.to_string())
            .or_default()
            .push((object_path.to_string(), cancel));

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
            objects.retain(|(p, _)| p != object_path);
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
            if args.new_owner().as_ref().map_or(true, |n| n.as_str().is_empty()) {
                let name = args.name().as_str();

                let objects_to_close = {
                    let mut state = self.state.lock().unwrap();
                    state.sender_objects.remove(name).unwrap_or_default()
                };

                for (path, cancel) in objects_to_close {
                    log::info!("Client {} disconnected, cancelling {}", name, path);
                    let _ = cancel.send(()).await;
                }
            }
        }
        Ok(())
    }
}
