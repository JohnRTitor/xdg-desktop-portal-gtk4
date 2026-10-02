use {
    crate::core::response::Response,
    std::{future::Future, sync::Arc},
    tokio::sync::Notify,
    zbus::{
        ObjectServer, interface,
        zvariant::{OwnedObjectPath, Type},
    },
};

/// Runs the future to completion or exits early if the request is closed.
///
/// This function sets up a race between the actual portal work (`f`) and the
/// cancellation listener on the Request D-Bus object. Whichever finishes first
/// determines the outcome. If cancellation wins, we return `Response::other()`.
///
/// The Request object is exported before registering with the
/// [`SessionManager`](crate::core::session_manager::SessionManager) so that it
/// exists on the bus for the whole life of the request. A `Close` that arrives
/// in the first moments of a request then reaches a real object and cancels it,
/// rather than arriving while the path is still unexported and coming back as
/// `UnknownObject`.
///
/// Note that the order is *not* what keeps a cancellation from being lost.
/// `Notify` latches a permit when `notify_one` runs with no waiter, so a
/// notification delivered before the `select!` below is armed is held until
/// something awaits it — see
/// `a_cancellation_before_the_select_is_still_observed` for that pinned
/// behaviour. Both the frontend's `Close` and the `SessionManager`'s
/// disconnect sweep notify this way.
pub async fn run_request<T, F>(
    server: &ObjectServer,
    session_manager: crate::core::session_manager::SessionManager,
    app_id: &str,
    sender: &str,
    handle: OwnedObjectPath,
    f: F,
) -> Response<T>
where
    T: Default + Type,
    F: Future<Output = Response<T>>,
{
    let notify = Arc::new(Notify::new());
    let cancel_notify = Arc::new(Notify::new());

    let request_exported = server
        .at(
            &handle,
            Request {
                notify: notify.clone(),
            },
        )
        .await
        .is_ok();

    let registered =
        session_manager.register(app_id, sender, handle.as_str(), cancel_notify.clone());

    let response = match &registered {
        // The app is already at its concurrent-request limit. Do not run the
        // portal work: export the Request (so a later Close() still succeeds)
        // and report the request as "other" immediately. Previously the error
        // was only logged and the work ran anyway, which made
        // `max_sessions_per_app` purely advisory.
        Err(e) => {
            tracing::warn!(
                "Rejecting request {} for {}: {}",
                handle.as_str(),
                app_id,
                e
            );
            Response::other()
        }
        Ok(()) => tokio::select! {
            v = f => v,
            // Requested by the frontend, or the caller vanished. Either way the
            // work never completed, which is "other", not "the user cancelled".
            _ = notify.notified() => Response::other(),
            _ = cancel_notify.notified() => Response::other(),
        },
    };

    if request_exported {
        let _ = server.remove::<Request, _>(&handle).await;
    }

    if registered.is_ok() {
        session_manager.unregister(app_id, sender, handle.as_str());
    }

    response
}

struct Request {
    notify: Arc<Notify>,
}

/// The implementation of the `org.freedesktop.impl.portal.Request` D-Bus interface.
#[interface(name = "org.freedesktop.impl.portal.Request")]
impl Request {
    /// Called by the portal frontend to cancel the ongoing request.
    async fn close(&self) {
        // Notify the `export_request` task that cancellation was requested.
        self.notify.notify_one();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_request_close() {
        let notify = Arc::new(Notify::new());
        let req = Request {
            notify: notify.clone(),
        };

        req.close().await;

        notify.notified().await; // Should complete immediately
    }

    /// A cancellation delivered before anyone starts listening is still seen.
    ///
    /// This is the property that makes the export/register order in
    /// [`run_request`] a robustness question rather than a lost-wakeup bug:
    /// `Notify` stores a permit when `notify_one` runs with no waiter, so a
    /// `Close` that lands in the window before the `select!` is armed is
    /// latched rather than dropped. It also covers the disconnect path, where
    /// `SessionManager` notifies from its own task.
    ///
    /// Pinned because the guarantee is a property of `Notify` rather than of
    /// this code: if a future refactor swapped `Notify` for a plain `AtomicBool`
    /// or an mpsc without a buffered permit, a cancellation could be lost and
    /// this test would be the thing that notices.
    #[tokio::test]
    async fn a_cancellation_before_the_select_is_still_observed() {
        let notify = Arc::new(Notify::new());
        notify.notify_one();
        // Let the notification happen strictly before the listener exists.
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        let seen =
            tokio::time::timeout(std::time::Duration::from_millis(500), notify.notified()).await;
        assert!(
            seen.is_ok(),
            "a notification sent before the listener existed must be latched"
        );
    }

    #[tokio::test]
    async fn test_run_request_completion() {
        let Ok(conn) = zbus::Connection::session().await else {
            return;
        };
        let server = conn.object_server();
        let sm = crate::core::session_manager::SessionManager::new(conn.clone(), 10);
        let handle =
            OwnedObjectPath::try_from("/org/freedesktop/portal/desktop/request/1").unwrap();

        let response: Response<u32> =
            run_request(server, sm, "test_app", "test_sender", handle, async {
                Response::success(42)
            })
            .await;

        assert_eq!(response.0, 0);
        assert_eq!(response.1, 42);
    }

    #[tokio::test]
    async fn test_run_request_cancellation() {
        let Ok(conn) = zbus::Connection::session().await else {
            return;
        };
        let server = conn.object_server();
        let sm = crate::core::session_manager::SessionManager::new(conn.clone(), 10);
        let handle =
            OwnedObjectPath::try_from("/org/freedesktop/portal/desktop/request/2").unwrap();

        let handle_clone = handle.clone();
        let conn_clone = conn.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            #[zbus::proxy(interface = "org.freedesktop.impl.portal.Request")]
            trait TestRequest {
                fn close(&self) -> zbus::Result<()>;
            }
            let proxy = TestRequestProxy::builder(&conn_clone)
                .destination(conn_clone.unique_name().unwrap().clone())
                .unwrap()
                .path(handle_clone)
                .unwrap()
                .build()
                .await
                .unwrap();
            let _ = proxy.close().await;
        });

        let response: Response<u32> =
            run_request(server, sm, "test_app", "test_sender", handle, async {
                tokio::time::sleep(std::time::Duration::from_secs(10)).await;
                Response::success(42)
            })
            .await;

        assert_eq!(response.0, 2); // 2 is "other": the request was closed, not user-cancelled
        assert_eq!(response.1, 0); // Default u32
    }
}
