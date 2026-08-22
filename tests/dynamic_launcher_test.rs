mod common;
use {
    common::*,
    std::collections::HashMap,
    xdg_desktop_portal_gtk4::portals::dynamic_launcher::dbus::DynamicLauncher,
    zbus::{
        connection::Builder,
        proxy,
        zvariant::{OwnedObjectPath, Value},
    },
};

#[proxy(
    interface = "org.freedesktop.impl.portal.DynamicLauncher",
    default_service = "org.freedesktop.impl.portal.desktop.gtk4",
    default_path = "/org/freedesktop/portal/desktop"
)]
trait DynamicLauncherPortal {
    fn prepare_install(
        &self,
        handle: OwnedObjectPath,
        app_id: &str,
        parent_window: &str,
        name: &str,
        icon_v: Value<'_>,
        options: HashMap<&str, Value<'_>>,
    ) -> zbus::Result<u32>;

    fn request_install_token(
        &self,
        app_id: &str,
        options: HashMap<&str, Value<'_>>,
    ) -> zbus::Result<u32>;
}

#[tokio::test]
async fn test_dynamic_launcher_prepare_install_dummy_ui() -> Result<(), Box<dyn std::error::Error>>
{
    let client_conn = try_dbus_session!();
    let sm = xdg_desktop_portal_gtk4::core::session_manager::SessionManager::new(
        client_conn.clone(),
        10,
    );
    let proxy_ui = dummy_proxy();
    let _conn = Builder::session()?
        .serve_at(
            "/org/freedesktop/portal/desktop",
            DynamicLauncher::new(&proxy_ui, sm),
        )?
        .build()
        .await?;

    let proxy = DynamicLauncherPortalProxy::builder(&client_conn)
        .destination(_conn.unique_name().unwrap().clone())?
        .build()
        .await?;

    let path = OwnedObjectPath::try_from("/org/freedesktop/portal/desktop/request/5").unwrap();
    let icon = Value::from("icon-name");

    let res = proxy
        .prepare_install(path, "app_id", "window", "name", icon, HashMap::new())
        .await;

    assert!(res.is_err()); // UI dummy proxy error

    // Also test request_install_token
    let token_res = proxy
        .request_install_token("app_id", HashMap::new())
        .await?;
    assert_eq!(token_res, 2); // default is denied

    Ok(())
}
