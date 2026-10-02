use {
    futures_util::stream::StreamExt,
    std::collections::HashMap,
    zbus::{
        connection::Builder,
        proxy,
        zvariant::{OwnedObjectPath, Value},
    },
};
mod common;
use xdg_desktop_portal_gtk4::portals::inhibit::dbus::Inhibit;

#[proxy(
    interface = "org.freedesktop.impl.portal.Inhibit",
    default_service = "org.freedesktop.impl.portal.desktop.gtk4",
    default_path = "/org/freedesktop/portal/desktop"
)]
trait InhibitTest {
    fn inhibit(
        &self,
        handle: OwnedObjectPath,
        app_id: &str,
        window: &str,
        reason: u32,
        options: HashMap<&str, Value<'_>>,
    ) -> zbus::Result<()>;

    fn create_monitor(
        &self,
        handle: OwnedObjectPath,
        session_handle: OwnedObjectPath,
        app_id: &str,
        window: &str,
    ) -> zbus::Result<u32>;

    #[zbus(signal)]
    fn state_changed(
        &self,
        session_handle: OwnedObjectPath,
        state: HashMap<&str, Value<'_>>,
    ) -> zbus::Result<()>;
}

#[tokio::test]
async fn test_inhibit_returns_success() -> Result<(), Box<dyn std::error::Error>> {
    let client_conn = try_dbus_session!();
    let _conn = Builder::session()?
        .serve_at(
            "/org/freedesktop/portal/desktop",
            Inhibit::new(
                xdg_desktop_portal_gtk4::core::session_manager::SessionManager::new(
                    client_conn.clone(),
                    10,
                ),
                None,
            )
            .await,
        )?
        .build()
        .await?;

    let proxy = InhibitTestProxy::builder(&client_conn)
        .destination(_conn.unique_name().unwrap().clone())?
        .build()
        .await?;

    let path = OwnedObjectPath::try_from("/org/freedesktop/portal/desktop/request/1").unwrap();
    proxy
        .inhibit(path, "app_id", "window", 1, HashMap::new())
        .await?;

    Ok(())
}

/// `CreateMonitor` must emit `StateChanged` once, immediately, so a new monitor
/// learns the current state instead of waiting for the next transition -- which
/// may never come if the state is steady. Without it, a monitor created while the
/// state is stable has no state at all until something changes.
///
/// The payload carries both `screensaver-active` and `session-state`, the two keys
/// the contract documents for the signal
/// (`org.freedesktop.impl.portal.Inhibit.xml`). The second matters because the
/// frontend reads it and re-emits the dict verbatim to the sandboxed app; when it
/// was missing, consumers saw the value `0`, outside the documented 1/2/3 range.
#[tokio::test]
async fn create_monitor_seeds_state_changed_with_both_keys()
-> Result<(), Box<dyn std::error::Error>> {
    let client_conn = try_dbus_session!();
    let _conn = Builder::session()?
        .serve_at(
            "/org/freedesktop/portal/desktop",
            Inhibit::new(
                xdg_desktop_portal_gtk4::core::session_manager::SessionManager::new(
                    client_conn.clone(),
                    10,
                ),
                None,
            )
            .await,
        )?
        .build()
        .await?;

    let proxy = InhibitTestProxy::builder(&client_conn)
        .destination(_conn.unique_name().unwrap().clone())?
        .build()
        .await?;

    let session_path =
        OwnedObjectPath::try_from("/org/freedesktop/portal/desktop/session/gtk4/1").unwrap();
    let handle = OwnedObjectPath::try_from("/org/freedesktop/portal/desktop/request/2").unwrap();

    // Subscribe before the call so the seeded emission cannot be missed.
    let mut states = proxy.receive_state_changed().await?;

    let response = proxy
        .create_monitor(handle, session_path.clone(), "org.example.App", "")
        .await?;
    assert_eq!(response, 0, "CreateMonitor must report success");

    let signal = tokio::time::timeout(std::time::Duration::from_secs(5), states.next())
        .await?
        .expect("CreateMonitor must emit an initial StateChanged");

    assert_eq!(signal.args()?.session_handle, session_path);
    let state = signal.args()?.state;
    assert_eq!(
        state.get("screensaver-active"),
        Some(&Value::Bool(false)),
        "no ActiveChanged has arrived, so the seeded value is false"
    );
    assert_eq!(
        state.get("session-state"),
        Some(&Value::U32(1)),
        "session-state must be present and Running, never absent"
    );

    Ok(())
}
