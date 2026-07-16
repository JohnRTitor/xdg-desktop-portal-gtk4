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

/// Runs the future to completion or exits early if the request is closed.
///
/// This function sets up a race between the actual portal work (`f`) and the
/// cancellation listener on the Request D-Bus object. Whichever finishes first
/// determines the outcome. If cancellation wins, we return `Response::cancelled()`.
///
/// This is inherently racy because the request might get cancelled before we export the
/// path. However, the portal frontend usually waits for the method reply before considering
/// the request fully established, so the race window is small.
pub async fn run_request<T, F>(server: &ObjectServer, handle: OwnedObjectPath, f: F) -> Response<T>
where
    T: Default + Type,
    F: Future<Output = Response<T>>,
{
    let (send, recv) = async_channel::bounded(1);
    let request_exported = server.at(&handle, Request { send }).await.is_ok();

    let response = select! {
        v = f.fuse() => v,
        _ = recv.recv().fuse() => Response::cancelled(),
    };

    if request_exported {
        let _ = server.remove::<Request, _>(&handle).await;
    }

    response
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
