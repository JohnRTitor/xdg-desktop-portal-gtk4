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
    pub async fn create(proxy: &UiProxy, replace: bool) -> Result<Self, PortalError> {
        let session = Connection::session()
            .await
            .map_err(PortalError::Connection)?;

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
        add!(Inhibit::new());
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

        let context = proxy.context.clone();
        context.spawn_local(async move {
            use futures_util::stream::StreamExt;
            if name_lost_iterator.next().await.is_some() {
                log::warn!("Lost name {}", NAME);
                std::process::exit(0);
            }
        });

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
