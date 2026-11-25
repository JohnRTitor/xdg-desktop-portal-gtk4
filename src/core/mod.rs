//! Core abstractions for the XDG Desktop Portal.
//!
//! This module provides the central infrastructure for D-Bus communication, session
//! management, and the Request/Response lifecycle.
//!
//! # Architecture
//!
//! The D-Bus objects (Portals, Requests, Sessions) all live within a Tokio async runtime.
//! When a portal receives a D-Bus method call, it typically:
//! 1. Starts a Tokio task to handle the request.
//! 2. Uses `crate::gui::run_ui_task` to dispatch any necessary GTK operations to the main thread.
//! 3. Creates a `Request` object on the D-Bus to allow the caller to track or cancel the operation.
//! 4. Yields a `Response` back over D-Bus when the GTK task completes.
//!
//! # Invariants
//!
//! - All `zbus` interfaces implemented in this crate must execute their methods
//!   without blocking the Tokio worker threads.
//! - The `Portal` struct owns a `Connection` handle. The D-Bus name is unregistered when the
//!   last handle (including clones held by spawned tasks) drops at runtime teardown.
//!
use {
    crate::gui::UiProxy,
    thiserror::Error,
    tokio::sync::oneshot::Sender as OneshotSender,
    zbus::{
        Connection,
        fdo::{DBusProxy, RequestNameFlags},
    },
};

#[cfg(test)]
use std::mem;

#[cfg(feature = "access")]
use crate::portals::access::dbus::Access;
#[cfg(feature = "account")]
use crate::portals::account::dbus::Account;
#[cfg(feature = "app_chooser")]
use crate::portals::app_chooser::dbus::AppChooser;
#[cfg(feature = "clipboard")]
use crate::portals::clipboard::dbus::ClipboardPortal;
#[cfg(feature = "dynamic_launcher")]
use crate::portals::dynamic_launcher::dbus::DynamicLauncher;
#[cfg(feature = "email")]
use crate::portals::email::dbus::Email;
#[cfg(feature = "file_chooser")]
use crate::portals::file_chooser::dbus::FileChooser;
#[cfg(feature = "inhibit")]
use crate::portals::inhibit::dbus::Inhibit;
#[cfg(feature = "lockdown")]
use crate::portals::lockdown::dbus::LockdownPortal;
#[cfg(feature = "notification")]
use crate::portals::notification::dbus::Notification;
#[cfg(all(feature = "print", target_os = "linux"))]
use crate::portals::print::dbus::Print;
#[cfg(feature = "settings")]
use crate::portals::settings::dbus::SettingsPortal;
#[cfg(feature = "usb")]
use crate::portals::usb::dbus::UsbPortal;

pub mod request;
pub mod response;
pub mod session;
pub mod session_manager;

const NAME: &str = "org.freedesktop.impl.portal.desktop.gtk4";
pub(crate) const DBUS_PATH: &str = "/org/freedesktop/portal/desktop";

#[derive(Debug, Error)]
pub enum PortalError {
    #[error("Could not connect to session bus")]
    Connection(#[source] Box<zbus::Error>),
    #[error("Could not acquire name {}", NAME)]
    AcquireName(#[source] Box<zbus::Error>),
    #[error("Could not add an interface")]
    AddInterface(#[source] Box<zbus::Error>),
    #[error("Could create dbus proxy")]
    CreateDbusProxy(#[source] Box<zbus::Error>),
    #[error("Could subscribe to name-lost events")]
    SubscribeNameLost(#[source] Box<zbus::Error>),
}

/// The core portal state, holding the main D-Bus connection.
///
/// This struct owns one handle to the underlying zbus `Connection`. The
/// `SessionManager` and several portals keep clones in spawned tasks, so the
/// D-Bus name is only unregistered when the last `Connection` handle drops as
/// the Tokio runtime shuts down; dropping a `Portal` alone does not tear the
/// connection down.
pub struct Portal {
    _session: Connection,
}

impl Portal {
    /// Creates the D-Bus interfaces and attempts to acquire the portal name.
    ///
    /// This method registers all specific portal implementations on the session bus.
    /// It must be called from within a Tokio async context, and typically runs on a
    /// dedicated background thread to prevent blocking the GTK main loop.
    pub async fn create(
        proxy: &UiProxy,
        replace: bool,
        name_lost_tx: OneshotSender<()>,
    ) -> Result<Self, PortalError> {
        let session = Connection::session()
            .await
            .map_err(|e| PortalError::Connection(Box::new(e)))?;

        let session_manager =
            crate::core::session_manager::SessionManager::new(session.clone(), 10);
        let session_manager_clone = session_manager.clone();

        tokio::spawn(async move {
            if let Err(e) = session_manager_clone.run().await {
                tracing::error!("SessionManager failed: {}", e);
            }
        });

        let system_conn = match Connection::system().await {
            Ok(conn) => Some(conn),
            Err(e) => {
                tracing::warn!(
                    "Could not connect to system bus: {}. Inhibit portal will not use logind.",
                    e
                );
                None
            }
        };

        macro_rules! add {
            ($interface:expr) => {
                session
                    .object_server()
                    .at(DBUS_PATH, $interface)
                    .await
                    .map_err(|e| PortalError::AddInterface(Box::new(e)))?;
            };
        }
        #[cfg(feature = "file_chooser")]
        add!(FileChooser::new(proxy, session_manager.clone()));
        #[cfg(feature = "email")]
        add!(Email::new(session_manager.clone()));
        #[cfg(feature = "access")]
        add!(Access::new(proxy, session_manager.clone()));
        #[cfg(feature = "account")]
        add!(Account::new(proxy, session_manager.clone()));
        #[cfg(feature = "notification")]
        add!(Notification::new(Some(session.clone())).await);
        #[cfg(feature = "dynamic_launcher")]
        add!(DynamicLauncher::new(proxy, session_manager.clone()));
        #[cfg(all(feature = "print", target_os = "linux"))]
        add!(Print::new(proxy, session_manager.clone()));
        #[cfg(feature = "inhibit")]
        add!(Inhibit::new(session_manager.clone(), system_conn).await);
        #[cfg(feature = "settings")]
        add!(SettingsPortal::new(proxy, session.object_server().clone()));
        #[cfg(feature = "lockdown")]
        add!(LockdownPortal::new());
        #[cfg(feature = "app_chooser")]
        add!(AppChooser::new(proxy, session_manager.clone()));
        #[cfg(feature = "usb")]
        add!(UsbPortal::new(proxy, session_manager.clone()));
        #[cfg(feature = "clipboard")]
        add!(ClipboardPortal::new(
            session.clone(),
            proxy.clone(),
            session_manager.clone()
        ));

        let mut name_lost_iterator = DBusProxy::new(&session)
            .await
            .map_err(|e| PortalError::CreateDbusProxy(Box::new(e)))?
            .receive_name_lost()
            .await
            .map_err(|e| PortalError::SubscribeNameLost(Box::new(e)))?;

        // Spawn a background task on Tokio to listen for name lost events.
        // If another process acquires our D-Bus name (e.g., another instance started with --replace),
        // we must exit cleanly. The portal specification expects the portal to go away if it loses its name.
        tokio::spawn(async move {
            use futures_util::stream::StreamExt;
            if name_lost_iterator.next().await.is_some() {
                tracing::warn!("Lost name {}", NAME);
                let _ = name_lost_tx.send(());
            }
        });

        // Request the D-Bus name.
        // `ALLOW_REPLACEMENT` means another instance can steal the name from us if it specifies `REPLACE_EXISTING`.
        // `DO_NOT_QUEUE` means we fail immediately if the name is already taken, instead of waiting in a queue.
        let mut flags = RequestNameFlags::ALLOW_REPLACEMENT | RequestNameFlags::DO_NOT_QUEUE;
        if replace {
            flags |= RequestNameFlags::REPLACE_EXISTING;
        }
        session
            .request_name_with_flags(NAME, flags)
            .await
            .map_err(|e| PortalError::AcquireName(Box::new(e)))?;
        tracing::info!("Portal ready on {NAME}");
        Ok(Self { _session: session })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Five variants holding only a thin `Box<zbus::Error>` still need an
    /// explicit discriminant, since one non-null niche cannot encode five
    /// states.
    #[test]
    #[cfg(target_pointer_width = "64")]
    fn portal_error_boxes_the_source() {
        assert_eq!(mem::size_of::<PortalError>(), 16);
    }
}
