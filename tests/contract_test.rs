//! Wire-contract regression tests for the portal implementations.
//!
//! Tests that currently pass are guards against future regressions.

use {
    std::collections::HashMap,
    xdg_desktop_portal_gtk4::gui::UiProxy,
    zbus::{
        Connection,
        zvariant::{ObjectPath, OwnedObjectPath, Value},
    },
};

fn dummy_proxy() -> UiProxy {
    UiProxy {
        context: gtk4::glib::MainContext::default(),
        sender: tokio::sync::mpsc::unbounded_channel().0,
    }
}

// ---------------------------------------------------------------- clipboard --

#[allow(clippy::too_many_arguments)]
#[zbus::proxy(
    interface = "org.freedesktop.impl.portal.Clipboard",
    default_path = "/org/freedesktop/portal/desktop"
)]
trait Clipboard {
    fn selection_write_done(
        &self,
        session: &ObjectPath<'_>,
        serial: u32,
        success: bool,
    ) -> zbus::Result<()>;
}

/// `SelectionWrite` / `SelectionWriteDone` / `SelectionRead` must validate the
/// session handle: serials are session-scoped, so a caller must not be able to
/// reach another session's pending transfer by guessing a serial.
///
/// * GNOME (`src/clipboard.c`, `handle_selection_write`) resolves
///   `session_handle` first and answers `org.freedesktop.portal.Error.NotFound`
///   for a non-existing session, `Failed` for a wrong session type.
/// * KDE (`ClipboardPortal::SelectionWrite`, `clipboard.cpp:327-349`) answers
///   `QDBusError::InvalidArgs` when the handle is not a clipboard-enabled
///   session, and `InvalidArgs` for an unknown serial.
///
/// The transfer map is additionally keyed by session, so validating the session
/// is not merely cosmetic: a caller that passes an unknown session but a real
/// serial still cannot reach the other session's descriptor.
#[tokio::test]
async fn clipboard_validates_session_handle() {
    let Ok(client) = Connection::session().await else {
        return;
    };
    let Ok(builder) = zbus::connection::Builder::session() else {
        return;
    };
    let server_conn = builder
        .serve_at(
            "/org/freedesktop/portal/desktop",
            xdg_desktop_portal_gtk4::portals::clipboard::dbus::ClipboardPortal::new(
                client.clone(),
                dummy_proxy(),
                xdg_desktop_portal_gtk4::core::session_manager::SessionManager::new(
                    client.clone(),
                    10,
                ),
            ),
        )
        .expect("serve_at");
    let _server: zbus::connection::Connection = server_conn.build().await.expect("private bus");

    let proxy = ClipboardProxy::builder(&client)
        .destination(_server.unique_name().unwrap().clone())
        .unwrap()
        .build()
        .await
        .unwrap();

    let bogus = ObjectPath::try_from("/org/freedesktop/portal/desktop/session/1/1").unwrap();
    let r = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        proxy.selection_write_done(&bogus, 4242, true),
    )
    .await
    .expect("SelectionWriteDone timed out");

    assert!(
        r.is_err(),
        "SelectionWriteDone must reject a session handle that never called RequestClipboard"
    );
}

// ------------------------------------------------------------- app chooser --

#[zbus::proxy(
    interface = "org.freedesktop.impl.portal.AppChooser",
    default_path = "/org/freedesktop/portal/desktop"
)]
trait AppChooser {
    fn update_choices(&self, handle: &OwnedObjectPath, choices: &[&str]) -> zbus::Result<()>;
}

/// `UpdateChoices` for a handle with no live dialog must fail, so that
/// frontend/backend desynchronisation is visible. `xdg-desktop-portal-gtk`
/// answers `org.freedesktop.portal.Error.NotFound` ("Request not found",
/// `appchooser.c:243-247`); KDE answers the standard `InvalidArgs`.
///
/// This previously returned `Ok(())` unconditionally, and the existing
/// `tests/app_chooser_test.rs` asserted that silent success as if it were
/// correct.
#[tokio::test]
async fn app_chooser_update_choices_unknown_handle() {
    let Ok(client) = Connection::session().await else {
        return;
    };
    let Ok(builder) = zbus::connection::Builder::session() else {
        return;
    };
    let server_conn = builder
        .serve_at(
            "/org/freedesktop/portal/desktop",
            xdg_desktop_portal_gtk4::portals::app_chooser::dbus::AppChooser::new(
                &dummy_proxy(),
                xdg_desktop_portal_gtk4::core::session_manager::SessionManager::new(
                    client.clone(),
                    10,
                ),
            ),
        )
        .expect("serve_at");
    let _server = server_conn.build().await.expect("private bus");

    let proxy = AppChooserProxy::builder(&client)
        .destination(_server.unique_name().unwrap().clone())
        .unwrap()
        .build()
        .await
        .unwrap();
    let handle = OwnedObjectPath::try_from("/org/freedesktop/portal/desktop/request/1/1").unwrap();
    let r = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        proxy.update_choices(&handle, &["a.desktop"]),
    )
    .await
    .expect("UpdateChoices timed out");
    assert!(
        r.is_err(),
        "UpdateChoices must report NotFound for a handle with no live dialog"
    );
}

// ------------------------------------------------------------- file chooser --

/// The public FileChooser spec (v4) for `current_filter` says: "If the filters
/// list is nonempty, it should match a filter in the list ... Alternatively, it
/// may be specified when the list is empty to apply the filter unconditionally."
///
/// xdg-desktop-portal-gtk implements both halves (`filechooser.c:554-595`) and
/// matches within a list by *name* rather than by value.
///
/// This exercises the real decision function, which previously could not be
/// reached from a test.
#[test]
fn file_chooser_current_filter_applies_without_filters_list() {
    use xdg_desktop_portal_gtk4::portals::file_chooser::gui::{
        Filter, FilterKind, effective_filters,
    };
    let text = Filter {
        name: "Text".into(),
        elements: vec![FilterKind::Glob("*.txt".into())],
    };
    let images = Filter {
        name: "Images".into(),
        elements: vec![FilterKind::Mime("image/png".into())],
    };

    // `current_filter` alone: applied unconditionally as the only filter.
    let (offered, selected) = effective_filters(None, Some(&text));
    assert_eq!(offered, vec![text.clone()]);
    assert_eq!(selected, Some(0));
    // An empty list behaves the same as an absent one.
    let (offered, selected) = effective_filters(Some(&[]), Some(&text));
    assert_eq!(offered, vec![text.clone()]);
    assert_eq!(selected, Some(0));

    // With a list, `current_filter` selects within it.
    let list = vec![images.clone(), text.clone()];
    let (offered, selected) = effective_filters(Some(&list), Some(&text));
    assert_eq!(offered, list);
    assert_eq!(selected, Some(1));

    // Matching is by name, so a `current_filter` that differs in its elements
    // still selects the list entry the application meant.
    let loose = Filter {
        name: "Text".into(),
        elements: vec![FilterKind::Glob("*.text".into())],
    };
    let (_, selected) = effective_filters(Some(&list), Some(&loose));
    assert_eq!(selected, Some(1));

    // A `current_filter` matching nothing in a non-empty list selects nothing.
    let unrelated = Filter {
        name: "Audio".into(),
        elements: vec![FilterKind::Mime("audio/*".into())],
    };
    let (_, selected) = effective_filters(Some(&list), Some(&unrelated));
    assert_eq!(selected, None);
}

// ------------------------------------------------------------------ account --

#[zbus::proxy(
    interface = "org.freedesktop.impl.portal.Account",
    default_path = "/org/freedesktop/portal/desktop"
)]
trait Account {
    fn get_user_information(
        &self,
        handle: &OwnedObjectPath,
        app_id: &str,
        window: &str,
        options: HashMap<&str, Value<'_>>,
    ) -> zbus::Result<(u32, HashMap<String, zbus::zvariant::OwnedValue>)>;
}

/// `Request.Close()` must be answered with a *non-cancellation* response code.
/// The frontend (`xdp-request.c:xdp_request_handle_close`) unexports the
/// Request object right after the backend's `Close` returns, so the value is
/// mostly cosmetic — but `xdg-desktop-portal-gtk` uses `2` ("other") for every
/// portal, and a backend that reports `1` ("user cancelled") makes a cancelled
/// request indistinguishable from a user rejection in logs and in
/// `xdg-desktop-portal`'s own bookkeeping.
#[tokio::test]
async fn request_close_reports_other_not_cancelled() {
    use xdg_desktop_portal_gtk4::core::request::run_request;

    let Ok(bus) = Connection::session().await else {
        return;
    };
    let sm = xdg_desktop_portal_gtk4::core::session_manager::SessionManager::new(bus.clone(), 10);
    let server = bus.object_server().clone();
    let handle = OwnedObjectPath::try_from("/org/freedesktop/portal/desktop/request/1/zz").unwrap();

    // Ask the backend to close the request, then wait for run_request to settle.
    #[zbus::proxy(interface = "org.freedesktop.impl.portal.Request")]
    trait RequestCloser {
        fn close(&self) -> zbus::Result<()>;
    }
    let closer = bus.clone();
    let dest = bus.unique_name().unwrap().to_string();
    let h = handle.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        let Ok(builder) = RequestCloserProxy::builder(&closer)
            .destination(dest.clone())
            .and_then(|b| b.path(h.as_str()))
        else {
            return;
        };
        let Ok(proxy) = builder.build().await else {
            return;
        };
        let _ = proxy.close().await;
    });

    let response: xdg_desktop_portal_gtk4::core::response::Response<u32> =
        run_request(&server, sm, "app", ":1.99", handle, async {
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            xdg_desktop_portal_gtk4::core::response::Response::success(1u32)
        })
        .await;

    assert_eq!(
        response.0, 2,
        "a request closed via Request.Close() must report response code 2 (other)"
    );
}
