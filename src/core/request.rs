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

async fn export_request(server: &ObjectServer, path: OwnedObjectPath) {
    let (send, recv) = async_channel::bounded(1);
    if let Err(e) = server.at(&path, Request { send }).await {
        log::error!("Could not export request object: {}", anyhow::Error::new(e));
        return;
    }
    let _ = recv.recv().await;
    let _ = server.remove::<Request, _>(&path).await;
}

/// Runs the future to completion or exits early if the request is closed.
///
/// This is inherently racy because the request might get cancelled before we export the
/// path.
pub async fn run_request<T, F>(server: &ObjectServer, handle: OwnedObjectPath, f: F) -> Response<T>
where
    T: Default + Type,
    F: Future<Output = Response<T>>,
{
    select! {
        v = f.fuse() => v,
        _ = export_request(server, handle).fuse() => Response::cancelled(),
    }
}

struct Request {
    send: Sender<()>,
}
#[interface(name = "org.freedesktop.impl.portal.Request")]
impl Request {
    async fn close(&self) {
        let _ = self.send.send(()).await;
    }
}

#[cfg(test)]
mod tests {
    use {super::*, std::time::Duration, zbus::Connection};

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

        // Yield a few times to let the spawn run and export the object
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;

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
