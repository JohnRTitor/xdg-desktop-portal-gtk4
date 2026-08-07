mod common;
use {
    common::*,
    std::collections::HashMap,
    xdg_desktop_portal_gtk4::portals::access::dbus::Access,
    zbus::{
        Connection,
        connection::Builder,
        proxy,
        zvariant::{OwnedObjectPath, Value},
    },
};

#[proxy(
    interface = "org.freedesktop.impl.portal.Access",
    default_service = "org.freedesktop.impl.portal.desktop.gtk4",
    default_path = "/org/freedesktop/portal/desktop"
)]
trait Access {
    fn access_dialog(
        &self,
        handle: OwnedObjectPath,
        app_id: &str,
        parent_window: &str,
        title: &str,
        subtitle: &str,
        body: &str,
        options: HashMap<&str, Value<'_>>,
    ) -> zbus::Result<u32>;
}

#[tokio::test]
async fn test_access_dialog_dummy_ui() -> Result<(), Box<dyn std::error::Error>> {
    skip_if_dbus_tests_disabled!();
    let client_conn = Connection::session().await?;
    let sm = xdg_desktop_portal_gtk4::core::session_manager::SessionManager::new(
        client_conn.clone(),
        10,
    );
    let proxy_ui = dummy_proxy();
    let _conn = Builder::session()?
        .serve_at(
            "/org/freedesktop/portal/desktop",
            Access::new(&proxy_ui, sm),
        )?
        .build()
        .await?;

    let proxy = AccessProxy::builder(&client_conn)
        .destination(_conn.unique_name().unwrap().clone())?
        .build()
        .await?;

    let path = OwnedObjectPath::try_from("/org/freedesktop/portal/desktop/request/1").unwrap();
    let res = proxy
        .access_dialog(
            path,
            "app_id",
            "window",
            "title",
            "subtitle",
            "body",
            HashMap::new(),
        )
        .await;

    // The dummy proxy immediately drops the channel, which should cause a UI cancellation or error.
    // As long as we get a response (even an error), it means D-Bus serialization succeeded.
    assert!(res.is_err());
    Ok(())
}
