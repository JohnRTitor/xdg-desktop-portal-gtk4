use {
    crate::core::response::Response,
    async_channel::Sender,
    futures_util::{FutureExt, select},
    std::future::Future,
    zbus::{
        ObjectServer, interface,
        zvariant::{OwnedObjectPath, Type},
    },
};

/// Exports a `Request` object on D-Bus and waits for it to be closed.
///
/// The `xdg-desktop-portal` frontend creates a request path and expects the backend
/// (us) to export an object at that path. The frontend can then call `Close()` on
/// this object to cancel the request.
async fn export_request(server: &ObjectServer, path: OwnedObjectPath) {
    let (send, recv) = async_channel::bounded(1);
    if let Err(e) = server.at(&path, Request { send }).await {
        log::error!("Could not export request object: {}", anyhow::Error::new(e));
        return;
    }
    // Wait until the frontend calls Close(), which sends a message through the channel.
    let _ = recv.recv().await;
    // Cleanup: remove the request object from the bus once closed.
    let _ = server.remove::<Request, _>(&path).await;
}

/// Runs the future to completion or exits early if the request is closed.
///
/// This function sets up a race between the actual portal work (`f`) and the
/// cancellation listener (`export_request`). Whichever finishes first determines
/// the outcome. If cancellation wins, we return `Response::cancelled()`.
///
/// This is inherently racy because the request might get cancelled before we export the
/// path. However, the portal frontend usually waits for the method reply before considering
/// the request fully established, so the race window is small.
pub async fn run_request<T, F>(server: &ObjectServer, handle: OwnedObjectPath, f: F) -> Response<T>
where
    T: Default + Type,
    F: Future<Output = Response<T>>,
{
    select! {
        // The actual work finished successfully or with an internal error.
        v = f.fuse() => v,
        // The frontend explicitly cancelled the request.
        _ = export_request(server, handle).fuse() => Response::cancelled(),
    }
}

struct Request {
    send: Sender<()>,
}

/// The implementation of the `org.freedesktop.impl.portal.Request` D-Bus interface.
#[interface(name = "org.freedesktop.impl.portal.Request")]
impl Request {
    /// Called by the portal frontend to cancel the ongoing request.
    async fn close(&self) {
        // Notify the `export_request` task that cancellation was requested.
        let _ = self.send.send(()).await;
    }
}

#[cfg(test)]
mod tests {
    use {super::*, zbus::Connection};

    #[tokio::test]
    async fn test_run_request_cancellation() -> Result<(), Box<dyn std::error::Error>> {
        if std::env::var("RUN_DBUS_TESTS").is_err() {
            println!("Skipping dbus test because RUN_DBUS_TESTS is not set");
            return Ok(());
        }

        let conn = Connection::session().await?;
        let unique_name = conn.unique_name().unwrap().clone();
        let server = conn.object_server();
        let path =
            OwnedObjectPath::try_from("/org/freedesktop/portal/desktop/request/test1").unwrap();

        let path_clone = path.clone();
        let server_clone = server.clone();

        let long_running = std::future::pending::<Response<()>>();

        let handle =
            tokio::spawn(async move { run_request(&server_clone, path_clone, long_running).await });

        // Sleep to let the spawn run and export the object
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let client_conn = Connection::session().await?;
        let proxy = zbus::Proxy::new(
            &client_conn,
            unique_name,
            "/org/freedesktop/portal/desktop/request/test1",
            "org.freedesktop.impl.portal.Request",
        )
        .await?;

        let _ = proxy.call_method("Close", &()).await?;

        let result = handle.await?;
        assert_eq!(result.0, 1); // PORTAL_CANCELLED is 1

        Ok(())
    }
}
