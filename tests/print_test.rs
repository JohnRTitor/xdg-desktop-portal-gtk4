mod common;
use {
    common::*,
    std::collections::HashMap,
    xdg_desktop_portal_gtk4::portals::print::dbus::Print,
    zbus::{
        Connection,
        connection::Builder,
        proxy,
        zvariant::{Fd, OwnedObjectPath, Value},
    },
};

#[proxy(
    interface = "org.freedesktop.impl.portal.Print",
    default_service = "org.freedesktop.impl.portal.desktop.gtk4",
    default_path = "/org/freedesktop/portal/desktop"
)]
trait PrintPortal {
    fn prepare_print(
        &self,
        handle: OwnedObjectPath,
        app_id: &str,
        parent_window: &str,
        title: &str,
        settings: HashMap<&str, Value<'_>>,
        page_setup: HashMap<&str, Value<'_>>,
        options: HashMap<&str, Value<'_>>,
    ) -> zbus::Result<u32>;

    fn print(
        &self,
        handle: OwnedObjectPath,
        app_id: &str,
        parent_window: &str,
        title: &str,
        fd: Fd<'_>,
        options: HashMap<&str, Value<'_>>,
    ) -> zbus::Result<u32>;
}

#[tokio::test]
async fn test_print_prepare_print_dummy_ui() -> Result<(), Box<dyn std::error::Error>> {
    skip_if_dbus_tests_disabled!();
    let client_conn = Connection::session().await?;
    let sm = xdg_desktop_portal_gtk4::core::session_manager::SessionManager::new(
        client_conn.clone(),
        10,
    );
    let proxy_ui = dummy_proxy();
    let _conn = Builder::session()?
        .serve_at("/org/freedesktop/portal/desktop", Print::new(&proxy_ui, sm))?
        .build()
        .await?;

    let proxy = PrintPortalProxy::builder(&client_conn)
        .destination(_conn.unique_name().unwrap().clone())?
        .build()
        .await?;

    let path = OwnedObjectPath::try_from("/org/freedesktop/portal/desktop/request/8").unwrap();
    let res = proxy
        .prepare_print(
            path,
            "app_id",
            "window",
            "title",
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
        )
        .await;

    assert!(res.is_err()); // UI dummy proxy error

    Ok(())
}
