use {
    crate::{
        gui::UiProxy,
        portals::{
            access::dbus::Access, account::dbus::Account, app_chooser::dbus::AppChooser,
            dynamic_launcher::dbus::DynamicLauncher, email::dbus::Email,
            file_chooser::dbus::FileChooser, inhibit::dbus::Inhibit,
            lockdown::dbus::LockdownPortal, notification::dbus::Notification, print::dbus::Print,
            settings::dbus::SettingsPortal, usb::dbus::UsbPortal,
        },
    },
    thiserror::Error,
    zbus::{Connection, fdo::RequestNameFlags},
};

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

        let session_manager = crate::core::session_manager::SessionManager::new(session.clone(), 10);
        let session_manager_clone = session_manager.clone();
        let context = proxy.context.clone();
        
        context.spawn_local(async move {
            if let Err(e) = session_manager_clone.run().await {
                log::error!("SessionManager failed: {}", e);
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
        add!(FileChooser::new(proxy));
        add!(Email::new());
        add!(Access::new(proxy));
        add!(Account::new(proxy));
        add!(Notification::new());
        add!(DynamicLauncher::new(proxy));
        add!(Print::new(proxy));
        add!(Inhibit::new(session_manager.clone()));
        add!(SettingsPortal::new(session.object_server().clone()));
        add!(LockdownPortal::new());
        add!(AppChooser::new(proxy));
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
                log::warn!("Lost name {}", NAME);
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
