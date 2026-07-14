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
