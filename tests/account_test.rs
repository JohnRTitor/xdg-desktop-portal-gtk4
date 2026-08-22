mod common;
use {
    common::*,
    std::collections::HashMap,
    xdg_desktop_portal_gtk4::portals::account::dbus::Account,
    zbus::{
        connection::Builder,
        proxy,
        zvariant::{OwnedObjectPath, Value},
    },
};

#[proxy(
    interface = "org.freedesktop.impl.portal.Account",
    default_service = "org.freedesktop.impl.portal.desktop.gtk4",
    default_path = "/org/freedesktop/portal/desktop"
)]
trait Account {
    fn get_user_information(
        &self,
        handle: OwnedObjectPath,
        app_id: &str,
        parent_window: &str,
        options: HashMap<&str, Value<'_>>,
    ) -> zbus::Result<u32>;
}

#[tokio::test]
async fn test_get_user_information_dummy_ui() -> Result<(), Box<dyn std::error::Error>> {
    let client_conn = try_dbus_session!();
    let sm = xdg_desktop_portal_gtk4::core::session_manager::SessionManager::new(
        client_conn.clone(),
        10,
    );
    let proxy_ui = dummy_proxy();
    let _conn = Builder::session()?
        .serve_at(
            "/org/freedesktop/portal/desktop",
            Account::new(&proxy_ui, sm),
        )?
        .build()
        .await?;

    let proxy = AccountProxy::builder(&client_conn)
        .destination(_conn.unique_name().unwrap().clone())?
        .build()
        .await?;

    let path = OwnedObjectPath::try_from("/org/freedesktop/portal/desktop/request/2").unwrap();
    let res = proxy
        .get_user_information(path, "app_id", "window", HashMap::new())
        .await;

    assert!(res.is_err());
    Ok(())
}
