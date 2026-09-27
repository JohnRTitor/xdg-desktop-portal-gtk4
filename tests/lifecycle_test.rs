//! Regression tests for the D-Bus request/session lifecycle and the
//! per-application session budget.
//!
//! These use a *real* D-Bus session bus and real client connections that are
//! connected and then dropped, so the `NameOwnerChanged` path inside
//! `SessionManager::run()` is genuinely exercised rather than mocked.

use {
    std::{sync::Arc, time::Duration},
    tokio::sync::Notify,
    xdg_desktop_portal_gtk4::core::session_manager::SessionManager,
    zbus::Connection,
};

const LIMIT: usize = 4;

async fn notified(n: &Notify) -> bool {
    tokio::select! {
        _ = n.notified() => true,
        _ = tokio::time::sleep(Duration::from_millis(1500)) => false,
    }
}

/// `SessionManager::run()` removes every entry for a departed sender *and*
/// decrements the per-app counter; `run_request()` then calls `unregister()`
/// for the very same `(app_id, path)` pair when the cancelled future finishes.
///
/// That double decrement must not free the per-app budget while the same
/// `app_id` still has live requests registered under a *different* sender.
/// Today it does, which makes `max_sessions_per_app` unenforceable in
/// exactly the situation it exists to protect against (a leaking or
/// misbehaving app).
#[tokio::test]
async fn disconnect_does_not_double_decrement_per_app_budget() {
    let Ok(bus) = Connection::session().await else {
        eprintln!("no session bus; skipping");
        return;
    };
    let Ok(s1) = Connection::session().await else {
        eprintln!("no second client connection; skipping");
        return;
    };
    let Ok(s2) = Connection::session().await else {
        eprintln!("no third client connection; skipping");
        return;
    };

    let sm = SessionManager::new(bus.clone(), LIMIT);
    let runner = sm.clone();
    tokio::spawn(async move {
        let _ = runner.run().await;
    });

    let name1 = s1.unique_name().unwrap().to_string();
    let name2 = s2.unique_name().unwrap().to_string();
    let (n1, n2) = (Arc::new(Notify::new()), Arc::new(Notify::new()));

    // Two different connections of the same application, one live request each.
    sm.register("app.a", &name1, "/p/1", n1.clone()).unwrap();
    sm.register("app.a", &name2, "/p/2", n2.clone()).unwrap();

    // Exhaust the rest of the budget.
    for i in 0..LIMIT - 2 {
        sm.register(
            "app.a",
            ":1.fake",
            &format!("/p/f{i}"),
            Arc::new(Notify::new()),
        )
        .unwrap();
    }
    assert!(
        sm.register("app.a", ":1.fake", "/p/overflow", Arc::new(Notify::new()))
            .is_err(),
        "budget of {LIMIT} must be exhausted before the disconnect"
    );

    // Let SessionManager::run() finish its AddMatch subscription before the
    // sender disappears, otherwise the signal is emitted before we listen.
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Sender #1 vanishes from the bus. The NameOwnerChanged sweep removes its
    // entry and releases exactly one budget slot for the app -- /p/1 really is
    // gone, so that slot legitimately comes back.
    drop(s1);
    assert!(
        notified(&n1).await,
        "sender disconnect must cancel its request"
    );

    // What run_request() does once the cancelled future unwinds. This must be a
    // no-op: the sweep already accounted for /p/1, and decrementing again is
    // what previously drove the app's count to zero and deleted the entry,
    // freeing the whole budget while /p/2 was still live.
    sm.unregister("app.a", &name1, "/p/1");

    // Exactly one slot came back, not two.
    assert!(
        sm.register("app.a", ":1.fake", "/p/reclaimed", Arc::new(Notify::new()))
            .is_ok(),
        "the one request lost to the disconnect must free exactly one budget slot"
    );
    assert!(
        sm.register("app.a", ":1.fake", "/p/overflow", Arc::new(Notify::new()))
            .is_err(),
        "per-app budget was freed while another request for the same app_id is still live"
    );
    let _ = n2;
}

/// The per-app limit must actually stop work, not merely log. `run_request()`
/// currently traces the `register()` failure and proceeds, so a caller can
/// issue unlimited concurrent dialogs.
#[tokio::test]
async fn session_limit_rejects_new_requests() {
    let Ok(bus) = Connection::session().await else {
        eprintln!("no session bus; skipping");
        return;
    };
    let sm = SessionManager::new(bus.clone(), 1);
    sm.register("app.b", ":1.x", "/existing", Arc::new(Notify::new()))
        .unwrap();
    for i in 0..4 {
        assert!(
            sm.register("app.b", ":1.y", &format!("/n{i}"), Arc::new(Notify::new()))
                .is_err(),
            "request {i} must be rejected while the app is at its limit"
        );
    }
}
