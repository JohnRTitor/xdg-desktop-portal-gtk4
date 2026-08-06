//! D-Bus session implementation.
//!
//! A Session represents a long-lived interaction between the sandbox and the portal,
//! typically used when the application needs continuous access to a resource (e.g.,
//! screen casting, remote desktop).

use {std::sync::Arc, tokio::sync::Notify, zbus::interface};

/// Represents a portal session on D-Bus.
///
/// Sessions are used by stateful portals (like ScreenCast, RemoteDesktop, etc.)
/// to manage ongoing interactions. The frontend can close the session, and the backend
/// can also close it.
///
/// # Ownership & Lifecycle
///
/// The `Session` struct is exported on the D-Bus via `zbus::ObjectServer`. It lives
/// as long as the D-Bus object is exported. When the session is closed (either by
/// the client over D-Bus or by the backend internally), the object is removed from
/// the server, which drops this struct.
///
/// If a session needs to clean up GTK resources when closed, it should use the `on_close`
/// notifier to signal a Tokio task that manages the GTK counterpart.
pub struct Session {
    pub id: String,
    pub on_close: Option<Arc<Notify>>,
}

impl Session {
    pub fn new(id: String, on_close: Option<Arc<Notify>>) -> Self {
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
        tracing::info!("Session {} closed", self.id);
        // We just notify that the session has been closed.
        if let Some(notify) = &self.on_close {
            notify.notify_one();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_session_close() {
        let notify = Arc::new(Notify::new());
        let session = Session::new("test_session_id".to_string(), Some(notify.clone()));

        assert_eq!(session.id, "test_session_id");

        session.close().await;

        notify.notified().await;
    }

    #[tokio::test]
    async fn test_session_close_no_channel() {
        let session = Session::new("test_session_id".to_string(), None);
        session.close().await; // Should not panic
    }
}
