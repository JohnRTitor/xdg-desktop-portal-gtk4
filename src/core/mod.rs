use {
    crate::gui::UiProxy,
    thiserror::Error,
    zbus::{Connection, fdo::RequestNameFlags},
};

#[cfg(feature = "access")]
use crate::portals::access::dbus::Access;
#[cfg(feature = "account")]
use crate::portals::account::dbus::Account;
#[cfg(feature = "app_chooser")]
use crate::portals::app_chooser::dbus::AppChooser;
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
#[cfg(feature = "print")]
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
const PATH: &str = "/org/freedesktop/portal/desktop";

#[derive(Debug, Error)]
pub enum PortalError {
    #[error("Could not connect to session bus")]
    Connection(#[source] zbus::Error),
    #[error("Could not acquire name {}", NAME)]
    AcquireName(#[source] zbus::Error),
    #[error("Could not add an interface")]
    AddInterface(#[source] zbus::Error),
    #[error("Could create dbus proxy")]
    CreateDbusProxy(#[source] zbus::Error),
    #[error("Could subscribe to name-lost events")]
    SubscribeNameLost(#[source] zbus::Error),
}

pub struct Portal {
    _session: Connection,
}

impl Portal {
    /// Creates the D-Bus interfaces and attempts to acquire the portal name.
    ///
    /// This method registers all specific portal implementations on the session bus.
    pub async fn create(proxy: &UiProxy, replace: bool) -> Result<Self, PortalError> {
        let session = Connection::session()
            .await
            .map_err(PortalError::Connection)?;

        let session_manager =
            crate::core::session_manager::SessionManager::new(session.clone(), 10);
        let session_manager_clone = session_manager.clone();
        let context = proxy.context.clone();

        context.spawn_local(async move {
            if let Err(e) = session_manager_clone.run().await {
                tracing::error!("SessionManager failed: {}", e);
            }
        });

        macro_rules! add {
            ($interface:expr) => {
                session
                    .object_server()
                    .at(PATH, $interface)
                    .await
                    .map_err(PortalError::AddInterface)?;
            };
        }
        #[cfg(feature = "file_chooser")]
        add!(FileChooser::new(proxy));
        #[cfg(feature = "email")]
        add!(Email::new());
        #[cfg(feature = "access")]
        add!(Access::new(proxy));
        #[cfg(feature = "account")]
        add!(Account::new(proxy));
        #[cfg(feature = "notification")]
        add!(Notification::new());
        #[cfg(feature = "dynamic_launcher")]
        add!(DynamicLauncher::new(proxy));
        #[cfg(feature = "print")]
        add!(Print::new(proxy));
        #[cfg(feature = "inhibit")]
        add!(Inhibit::new(session_manager.clone()));
        #[cfg(feature = "settings")]
        add!(SettingsPortal::new(session.object_server().clone()));
        #[cfg(feature = "lockdown")]
        add!(LockdownPortal::new());
        #[cfg(feature = "app_chooser")]
        add!(AppChooser::new(proxy));
        #[cfg(feature = "usb")]
        add!(UsbPortal::new(proxy));

        let mut name_lost_iterator = zbus::fdo::DBusProxy::new(&session)
            .await
            .map_err(PortalError::CreateDbusProxy)?
            .receive_name_lost()
            .await
            .map_err(PortalError::SubscribeNameLost)?;

        // Spawn a background task on the GTK MainContext to listen for name lost events.
        // If another process acquires our D-Bus name (e.g., another instance started with --replace),
        // we must exit cleanly. The portal specification expects the portal to go away if it loses its name.
        context.spawn_local(async move {
            use futures_util::stream::StreamExt;
            if name_lost_iterator.next().await.is_some() {
                tracing::warn!("Lost name {}", NAME);
                std::process::exit(0);
            }
        });

        // Request the D-Bus name.
        // `AllowReplacement` means another instance can steal the name from us if it specifies `ReplaceExisting`.
        // `DoNotQueue` means we fail immediately if the name is already taken, instead of waiting in a queue.
        let mut flags = RequestNameFlags::AllowReplacement | RequestNameFlags::DoNotQueue;
        if replace {
            flags |= RequestNameFlags::ReplaceExisting;
        }
        session
            .request_name_with_flags(NAME, flags)
            .await
            .map_err(PortalError::AcquireName)?;
        Ok(Self { _session: session })
    }
}
