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
/// as long as the D-Bus object is exported, and is dropped when whoever created it
/// unexports the object — `close()` itself does not unexport, so a caller that
/// wants the object gone must remove it. The Inhibit portal does both: it passes
/// an `on_close` notifier here and unexports the session in the task that notifier
/// wakes.
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
    ///
    /// The `Closed` signal declared by this interface is deliberately not emitted
    /// here. Per `data/org.freedesktop.impl.portal.Session.xml` it reports a
    /// session the *backend* aborted on its own initiative, and the frontend
    /// subscribes to it in `on_closed()` (`xdp-session.c`), re-emitting `Closed`
    /// to the application from there. An application-initiated `Close` never
    /// travels that way: the frontend handles it in `handle_close()`, which calls
    /// `xdp_session_close(session, FALSE)` — no signal to the app — and only then
    /// calls this method. Signalling from here would therefore be redundant, and
    /// would mean something other than what the contract says.
    ///
    /// Note that some backends (KDE, COSMIC, Luminous) do emit it here anyway.
    /// That is tolerated rather than correct: the frontend's `session->closed`
    /// guard makes the extra signal a no-op. This backend has no autonomous
    /// abort path to report, so it has nothing to emit.
    ///
    /// Unexporting is left to the creator — see the type-level docs.
    async fn close(&self) {
        tracing::info!("Session {} closed", self.id);
        if let Some(notify) = &self.on_close {
            notify.notify_one();
        }
    }

    /// Interface version, declared by
    /// `data/org.freedesktop.impl.portal.Session.xml`.
    ///
    /// 1, because the only method here is `Close`. The contract's version 2
    /// replaces `Close` with `Close2`, which this backend does not implement, so
    /// claiming 2 would invite a frontend onto a method that does not exist.
    ///
    /// The wire name needs the explicit `name = "version"`: `#[zbus(property)]`
    /// otherwise derives it from the Rust fn name and capitalises it, exporting
    /// `Version`. The frontend reads `version`. `Clipboard` and `Settings` both
    /// shipped that drift before
    /// `tests/introspection_test.rs::session_interface_matches_upstream_contract`
    /// was written to catch it.
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

    /// Pins the returned value only.
    ///
    /// This is *not* a guard on the wire name: drop `name = "version"` and this
    /// still passes while the property is exported as `Version` and the frontend
    /// reads nothing. That is the defect `Clipboard` and `Settings` both shipped.
    /// The guard for it is
    /// `tests/introspection_test.rs::session_interface_matches_upstream_contract`.
    #[test]
    fn version_is_one() {
        assert_eq!(Session::new("s".into(), None).version(), 1);
    }
}
