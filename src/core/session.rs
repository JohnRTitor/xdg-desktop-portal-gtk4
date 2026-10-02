//! D-Bus session implementation.
//!
//! A Session represents a long-lived interaction between the sandbox and the portal,
//! typically used when the application needs continuous access to a resource (e.g.,
//! screen casting, remote desktop).

use {
    std::sync::Arc,
    tokio::sync::Notify,
    zbus::{ObjectServer, interface, object_server::SignalEmitter},
};

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
    async fn close(
        &self,
        #[zbus(object_server)] server: &ObjectServer,
        #[zbus(signal_emitter)] signal_emitter: SignalEmitter<'_>,
    ) {
        tracing::info!("Session {} closed", self.id);

        let _ = Self::closed(&signal_emitter).await;

        if let Some(notify) = &self.on_close {
            notify.notify_one();
        }

        let _ = server.remove::<Session, _>(self.id.as_ref()).await;
    }

    #[zbus(signal)]
    async fn closed(ctxt: &SignalEmitter<'_>) -> zbus::Result<()>;
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
        let Ok(conn) = zbus::Connection::session().await else {
            return;
        };
        let server = conn.object_server();
        let path = zbus::zvariant::ObjectPath::try_from("/test/session/1").unwrap();

        let notify = Arc::new(Notify::new());
        let session = Session::new("/test/session/1".into(), Some(notify.clone()));

        server.at(&path, session).await.unwrap();

        let iface_ref = server.interface::<_, Session>(&path).await.unwrap();
        let emitter = iface_ref.signal_emitter();
        let session_ref = iface_ref.get().await;

        Session::close(&session_ref, server, emitter.clone()).await;

        notify.notified().await;
    }

    #[tokio::test]
    async fn test_session_close_no_channel() {
        let Ok(conn) = zbus::Connection::session().await else {
            return;
        };
        let server = conn.object_server();
        let path = zbus::zvariant::ObjectPath::try_from("/test/session/2").unwrap();

        let session = Session::new("/test/session/2".into(), None);

        server.at(&path, session).await.unwrap();

        let iface_ref = server.interface::<_, Session>(&path).await.unwrap();
        let emitter = iface_ref.signal_emitter();
        let session_ref = iface_ref.get().await;

        Session::close(&session_ref, server, emitter.clone()).await;
    }
}
