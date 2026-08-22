mod common;
use {
    common::*,
    std::collections::HashMap,
    xdg_desktop_portal_gtk4::portals::usb::dbus::UsbPortal,
    zbus::{
        connection::Builder,
        proxy,
        zvariant::{OwnedObjectPath, OwnedValue},
    },
};

#[proxy(
    interface = "org.freedesktop.impl.portal.Usb",
    default_service = "org.freedesktop.impl.portal.desktop.gtk4",
    default_path = "/org/freedesktop/portal/desktop"
)]
trait Usb {
    #[zbus(name = "AcquireDevices")]
    fn acquire_devices(
        &self,
        handle: OwnedObjectPath,
        parent_window: &str,
        app_id: &str,
        devices: Vec<(
            String,
            HashMap<String, OwnedValue>,
            HashMap<String, OwnedValue>,
        )>,
        options: HashMap<String, OwnedValue>,
    ) -> zbus::Result<u32>;
}

#[tokio::test]
async fn test_usb_acquire_devices_dummy_ui() -> Result<(), Box<dyn std::error::Error>> {
    let client_conn = try_dbus_session!();
    let sm = xdg_desktop_portal_gtk4::core::session_manager::SessionManager::new(
        client_conn.clone(),
        10,
    );
    let proxy_ui = dummy_proxy();
    let _conn = Builder::session()?
        .serve_at(
            "/org/freedesktop/portal/desktop",
            UsbPortal::new(&proxy_ui, sm),
        )?
        .build()
        .await?;

    let proxy = UsbProxy::builder(&client_conn)
        .destination(_conn.unique_name().unwrap().clone())?
        .build()
        .await?;

    let path = OwnedObjectPath::try_from("/org/freedesktop/portal/desktop/request/9").unwrap();
    let res = proxy
        .acquire_devices(path, "window", "app_id", vec![], HashMap::new())
        .await;

    assert!(res.is_err()); // UI dummy proxy error

    Ok(())
}
