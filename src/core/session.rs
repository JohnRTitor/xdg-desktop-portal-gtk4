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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_session_close() {
        let (send, recv) = async_channel::bounded(1);
        let session = Session::new("test_session_id".to_string(), Some(send));

        assert_eq!(session.id, "test_session_id");

        session.close().await;

        let received = recv.try_recv().unwrap();
        assert_eq!(received, "test_session_id");
    }

    #[tokio::test]
    async fn test_session_close_no_channel() {
        let session = Session::new("test_session_id".to_string(), None);
        session.close().await; // Should not panic
    }
}
