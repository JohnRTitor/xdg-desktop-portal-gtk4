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
    /// The session's object path.
    pub id: Box<str>,
    pub on_close: Option<Arc<Notify>>,
}

impl Session {
    pub fn new(id: Box<str>, on_close: Option<Arc<Notify>>) -> Self {
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

    /// Interface version, declared by
    /// `data/org.freedesktop.impl.portal.Session.xml`.
    ///
    /// The `Closed` signal declared by the same interface is deliberately not
    /// emitted from [`Self::close`]. It reports a session the *backend* aborted
    /// on its own initiative: the frontend subscribes to it in `on_closed()`
    /// (`xdp-session.c`) and re-emits `Closed` to the application from there.
    /// An application-initiated `Close` never travels that way — the frontend
    /// handles it in `handle_close()` and only then calls the backend's
    /// `Close` — so signalling from this handler would be redundant. This
    /// backend has no autonomous abort path to report, which is why
    /// `xdg-desktop-portal-gtk` emits no signal here either.
    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(target_pointer_width = "64")]
    fn id_is_a_boxed_str() {
        assert_eq!(std::mem::size_of::<Session>(), 24);
    }

    #[tokio::test]
    async fn test_session_close() {
        let notify = Arc::new(Notify::new());
        let session = Session::new("test_session_id".into(), Some(notify.clone()));

        assert_eq!(&*session.id, "test_session_id");

        session.close().await;

        notify.notified().await;
    }

    #[tokio::test]
    async fn test_session_close_no_channel() {
        let session = Session::new("test_session_id".into(), None);
        session.close().await; // Should not panic
    }

    /// The interface declares `version` as a read-only `u`. A pure getter, so
    /// this needs no bus and cannot pass vacuously.
    #[test]
    fn version_is_one() {
        assert_eq!(Session::new("s".into(), None).version(), 1);
    }
}
