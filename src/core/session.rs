use zbus::interface;

/// Represents a portal session on D-Bus.
///
/// Sessions are used by stateful portals (like ScreenCast, RemoteDesktop, etc.)
/// to manage ongoing interactions. The frontend can close the session, and the backend
/// can also close it.
pub struct Session {
    pub id: String,
    pub on_close: Option<async_channel::Sender<String>>,
}

impl Session {
    pub fn new(id: String, on_close: Option<async_channel::Sender<String>>) -> Self {
        Self { id, on_close }
    }
}

/// The implementation of the `org.freedesktop.impl.portal.Session` D-Bus interface.
#[interface(name = "org.freedesktop.impl.portal.Session")]
impl Session {
    /// Called by the portal frontend to close the session.
    async fn close(&self) {
        // Currently, we only log the closure. Real implementations (if added later)
        // would need to clean up resources, close GTK dialogs, or stop screen recording.
        log::info!("Session {} closed", self.id);
        if let Some(tx) = &self.on_close {
            let _ = tx.send(self.id.clone()).await;
        }
    }
}
