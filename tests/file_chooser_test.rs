mod common;
use {
    common::*,
    std::collections::HashMap,
    xdg_desktop_portal_gtk4::portals::file_chooser::dbus::FileChooser,
    zbus::{
        connection::Builder,
        proxy,
        zvariant::{OwnedObjectPath, Value},
    },
};

#[proxy(
    interface = "org.freedesktop.impl.portal.FileChooser",
    default_service = "org.freedesktop.impl.portal.desktop.gtk4",
    default_path = "/org/freedesktop/portal/desktop"
)]
trait FileChooserPortal {
    fn open_file(
        &self,
        handle: OwnedObjectPath,
        app_id: &str,
        parent_window: &str,
        title: &str,
        options: HashMap<&str, Value<'_>>,
    ) -> zbus::Result<u32>;

    fn save_file(
        &self,
        handle: OwnedObjectPath,
        app_id: &str,
        parent_window: &str,
        title: &str,
        options: HashMap<&str, Value<'_>>,
    ) -> zbus::Result<u32>;

    fn save_files(
        &self,
        handle: OwnedObjectPath,
        app_id: &str,
        parent_window: &str,
        title: &str,
        options: HashMap<&str, Value<'_>>,
    ) -> zbus::Result<u32>;
}

#[tokio::test]
async fn test_file_chooser_open_file_dummy_ui() -> Result<(), Box<dyn std::error::Error>> {
    let client_conn = try_dbus_session!();
    let sm = xdg_desktop_portal_gtk4::core::session_manager::SessionManager::new(
        client_conn.clone(),
        10,
    );
    let proxy_ui = dummy_proxy();
    let _conn = Builder::session()?
        .serve_at(
            "/org/freedesktop/portal/desktop",
            FileChooser::new(&proxy_ui, sm),
        )?
        .build()
        .await?;

    let proxy = FileChooserPortalProxy::builder(&client_conn)
        .destination(_conn.unique_name().unwrap().clone())?
        .build()
        .await?;

    let path = OwnedObjectPath::try_from("/org/freedesktop/portal/desktop/request/6").unwrap();
    let res = proxy
        .open_file(path, "app_id", "window", "title", HashMap::new())
        .await;

    assert!(res.is_err()); // UI dummy proxy error

    Ok(())
}

#[tokio::test]
async fn test_file_chooser_save_file_dummy_ui() -> Result<(), Box<dyn std::error::Error>> {
    let client_conn = try_dbus_session!();
    let sm = xdg_desktop_portal_gtk4::core::session_manager::SessionManager::new(
        client_conn.clone(),
        10,
    );
    let proxy_ui = dummy_proxy();
    let _conn = Builder::session()?
        .serve_at(
            "/org/freedesktop/portal/desktop",
            FileChooser::new(&proxy_ui, sm),
        )?
        .build()
        .await?;

    let proxy = FileChooserPortalProxy::builder(&client_conn)
        .destination(_conn.unique_name().unwrap().clone())?
        .build()
        .await?;

    let path = OwnedObjectPath::try_from("/org/freedesktop/portal/desktop/request/7").unwrap();
    let res = proxy
        .save_file(path, "app_id", "window", "title", HashMap::new())
        .await;

    assert!(res.is_err()); // UI dummy proxy error

    Ok(())
}
