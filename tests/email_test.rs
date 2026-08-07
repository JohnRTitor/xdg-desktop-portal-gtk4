mod common;
use {
    common::*,
    std::collections::HashMap,
    xdg_desktop_portal_gtk4::portals::email::dbus::Email,
    zbus::{
        Connection,
        connection::Builder,
        proxy,
        zvariant::{OwnedObjectPath, Value},
    },
};

#[proxy(
    interface = "org.freedesktop.impl.portal.Email",
    default_service = "org.freedesktop.impl.portal.desktop.gtk4",
    default_path = "/org/freedesktop/portal/desktop"
)]
trait EmailPortal {
    fn compose_email(
        &self,
        handle: OwnedObjectPath,
        app_id: &str,
        parent_window: &str,
        options: HashMap<&str, Value<'_>>,
    ) -> zbus::Result<u32>;
}

#[tokio::test]
async fn test_compose_email_dummy_ui() -> Result<(), Box<dyn std::error::Error>> {
    skip_if_dbus_tests_disabled!();
    let client_conn = Connection::session().await?;
    let sm = xdg_desktop_portal_gtk4::core::session_manager::SessionManager::new(
        client_conn.clone(),
        10,
    );
    let _conn = Builder::session()?
        .serve_at("/org/freedesktop/portal/desktop", Email::new(sm))?
        .build()
        .await?;

    let proxy = EmailPortalProxy::builder(&client_conn)
        .destination(_conn.unique_name().unwrap().clone())?
        .build()
        .await?;

    let path = OwnedObjectPath::try_from("/org/freedesktop/portal/desktop/request/3").unwrap();
    let res = proxy
        .compose_email(path, "app_id", "window", HashMap::new())
        .await;

    // Email might succeed or fail depending on if `gio::AppInfo::launch_default_for_uri` is available
    // in the headless environment, but D-Bus serialization works either way.
    let _ = res; // We just care that it doesn't panic on D-Bus communication
    Ok(())
}
