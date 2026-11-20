#![allow(clippy::too_many_arguments)] // proxy macro generates 8-arg client methods

mod common;
use {
    common::*,
    std::collections::HashMap,
    xdg_desktop_portal_gtk4::portals::print::dbus::Print,
    zbus::{
        connection::Builder,
        proxy,
        zvariant::{Fd, OwnedObjectPath, OwnedValue, Value},
    },
};

/// The impl portal returns `Response<T>`, serialized as `(u, a{sv})` — not a
/// bare `u`. Declaring the wrong return type made the old test pass for the
/// wrong reason (deserialization failed regardless of what the portal did).
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
    ) -> zbus::Result<(u32, HashMap<String, OwnedValue>)>;

    fn print(
        &self,
        handle: OwnedObjectPath,
        app_id: &str,
        parent_window: &str,
        title: &str,
        fd: Fd<'_>,
        options: HashMap<&str, Value<'_>>,
    ) -> zbus::Result<(u32, HashMap<String, OwnedValue>)>;
}

#[tokio::test]
async fn test_print_prepare_print_dummy_ui() -> Result<(), Box<dyn std::error::Error>> {
    let client_conn = try_dbus_session!();
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
    let response = proxy
        .prepare_print(
            path,
            "app_id",
            "window",
            "title",
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
        )
        .await?;

    assert_eq!(
        response.0, 2,
        "a UI that never opened is 'other', not cancellation"
    );

    Ok(())
}

/// Token 0 is what `GtkPrintDialog.print_file()` sends with a NULL setup. The
/// portal must handle that call itself instead of rejecting it as unknown. The
/// dummy UI cannot open a dialog, so the expected response is "other" (2); the
/// important part is that the method completes and answers with a real code.
#[tokio::test]
async fn test_print_single_call_dummy_ui() -> Result<(), Box<dyn std::error::Error>> {
    let client_conn = try_dbus_session!();
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

    let path = OwnedObjectPath::try_from("/org/freedesktop/portal/desktop/request/9").unwrap();
    let file = tempfile::tempfile()?;
    let response = proxy
        .print(
            path,
            "app_id",
            "window",
            "title",
            Fd::from(&file),
            HashMap::new(),
        )
        .await?;

    assert_eq!(
        response.0, 2,
        "the dummy UI must answer with a real response code"
    );

    Ok(())
}
