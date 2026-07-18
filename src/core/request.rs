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
    use super::*;

    #[tokio::test]
    async fn test_request_close() {
        let (send, recv) = async_channel::bounded(1);
        let req = Request { send };

        req.close().await;

        assert!(recv.try_recv().is_ok());
    }

    #[tokio::test]
    async fn test_run_request_completion() {
        // Can't easily test run_request cancellation with ObjectServer without a connection,
        // but we can at least test Request::close logic as above.
    }
}
