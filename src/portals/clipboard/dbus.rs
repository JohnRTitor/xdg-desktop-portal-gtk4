//! D-Bus implementation of the Clipboard portal.
//!
//! This module coordinates clipboard access between sandboxed applications and the host GTK
//! environment. It heavily relies on passing file descriptors (FDs) over D-Bus to stream
//! clipboard content without buffering large amounts of data in the portal memory.
//!
//! # Threading Model
//! The D-Bus interface methods are executed by zbus on Tokio threads. However, clipboard
//! interaction strictly requires GTK main thread access. Thus, requests are often routed
//! through `UiProxy` to the GTK thread.

use {
    crate::{
        gui::{PortalDispatcher, UiProxy},
        portals::clipboard::gtk_backend,
    },
    gtk4::glib::MainContext,
    parking_lot::Mutex,
    std::{
        collections::HashMap,
        os::fd::OwnedFd,
        sync::{
            Arc,
            atomic::{AtomicU32, Ordering},
        },
        time::Duration,
    },
    tokio::sync::{
        Notify,
        oneshot::{Sender, channel},
    },
    zbus::{
        Connection, fdo, interface,
        message::Header,
        object_server::SignalEmitter,
        zvariant::{Fd, ObjectPath, Value},
    },
};

struct TransferRequest {
    fd_sender: Sender<OwnedFd>,
}

/// D-Bus interface wrapper for the Clipboard portal.
///
/// This struct holds the shared state for clipboard operations, notably managing
/// the active sessions (which need to be notified of host clipboard changes) and
/// pending file descriptor transfers.
pub struct ClipboardPortal {
    /// Tracks active sessions that have requested clipboard access.
    /// We emit `SelectionOwnerChanged` signals to all these sessions when the host clipboard changes.
    active_sessions: Arc<Mutex<Vec<ObjectPath<'static>>>>,

    /// Maps a unique serial number to an active transfer request.
    /// When the host wants to read from a sandboxed app, we generate a serial, pass it to the app
    /// via `SelectionTransfer`, and when the app calls `SelectionWrite` with that serial, we map
    /// it back to the `fd_sender` to provide the writing end of a pipe.
    pending_transfers: Arc<Mutex<HashMap<u32, TransferRequest>>>,

    connection: Connection,
    proxy: UiProxy,
    session_manager: crate::core::session_manager::SessionManager,
}

impl ClipboardPortal {
    pub fn new(
        connection: Connection,
        proxy: UiProxy,
        session_manager: crate::core::session_manager::SessionManager,
    ) -> Self {
        let pending_transfers = Arc::new(Mutex::new(HashMap::new()));
        let active_sessions = Arc::new(Mutex::new(Vec::new()));

        let conn_clone = connection.clone();
        let sessions_clone = active_sessions.clone();

        let (tx, rx) = channel();

        // Run GTK-specific initialization on the main thread and pipe the event stream back to Tokio
        let _ = proxy.sender.send(Box::new(move || {
            MainContext::default().spawn_local(async move {
                match gtk_backend::subscribe_changes() {
                    Ok(formats_rx) => {
                        let _ = tx.send(formats_rx);
                    }
                    Err(e) => {
                        tracing::warn!("Clipboard portal backend unavailable: {}", e);
                    }
                }
            });
        }));

        // Process GTK events and emit D-Bus signals entirely on the Tokio background thread
        // to avoid bogging down the GTK main loop. The GTK thread sends us updates via `rx`.
        tokio::spawn(async move {
            let Ok(mut formats_rx) = rx.await else {
                return;
            };

            while let Ok(mimes) = formats_rx.recv().await {
                let emitter = match SignalEmitter::new(&conn_clone, crate::core::DBUS_PATH) {
                    Ok(e) => e,
                    Err(err) => {
                        tracing::error!("Failed to create SignalEmitter: {}", err);
                        return;
                    }
                };

                let mut options = HashMap::new();
                let mimes_val = Value::from(mimes.clone());
                options.insert("mime_types", &mimes_val);
                let is_owner = Value::from(false);
                options.insert("session_is_owner", &is_owner);

                let sessions = sessions_clone.lock().clone();
                for session in sessions {
                    let _ =
                        Self::selection_owner_changed(&emitter, &session, options.clone()).await;
                }
            }
        });

        Self {
            active_sessions,
            pending_transfers,
            connection,
            proxy,
            session_manager,
        }
    }
}

#[interface(name = "org.freedesktop.impl.portal.Clipboard")]
impl ClipboardPortal {
    async fn request_clipboard(
        &self,
        #[zbus(header)] header: Header<'_>,
        session_handle: ObjectPath<'_>,
        _options: HashMap<&str, Value<'_>>,
    ) -> fdo::Result<()> {
        let sender = header
            .sender()
            .map(|s| String::from(s.as_str()))
            .ok_or_else(|| fdo::Error::Failed("Missing sender".into()))?;

        tracing::debug!("RequestClipboard called for session: {:?}", session_handle);
        let session_handle_owned = session_handle.into_owned();
        {
            let mut sessions = self.active_sessions.lock();
            if !sessions.contains(&session_handle_owned) {
                sessions.push(session_handle_owned.clone());
            }
        }

        let cancel_notify = Arc::new(Notify::new());
        if let Err(e) = self.session_manager.register(
            "clipboard", // app_id isn't directly available, but we can use "clipboard" or just skip rate limiting
            &sender,
            session_handle_owned.as_str(),
            cancel_notify.clone(),
        ) {
            tracing::warn!("Session limit exceeded for clipboard: {}", e);
            // Even if it fails, we continue, but we won't clean up automatically
        } else {
            let active_sessions_clone = self.active_sessions.clone();
            let session_handle_clone = session_handle_owned.clone();
            let session_manager_clone = self.session_manager.clone();
            tokio::spawn(async move {
                cancel_notify.notified().await;
                tracing::debug!(
                    "App {} disconnected, cleaning up clipboard session {:?}",
                    sender,
                    session_handle_clone
                );
                active_sessions_clone
                    .lock()
                    .retain(|s| s != &session_handle_clone);
                session_manager_clone.unregister(
                    "clipboard",
                    &sender,
                    session_handle_clone.as_str(),
                );
            });
        }

        let conn_clone = self.connection.clone();
        let mimes = crate::gui::run_ui_task(
            &self.proxy,
            |tx, _, _| {
                let mimes = gtk_backend::current_formats().unwrap_or_default();
                let _ = tx.dispatch(Ok::<_, fdo::Error>(mimes));
            },
            || fdo::Error::Failed("UI task cancelled".into()),
        )
        .await
        .unwrap_or_default();

        if let Ok(emitter) = SignalEmitter::new(&conn_clone, crate::core::DBUS_PATH) {
            let mut options = HashMap::new();
            let mimes_val = Value::from(mimes);
            options.insert("mime_types", &mimes_val);
            let is_owner = Value::from(false);
            options.insert("session_is_owner", &is_owner);
            tracing::debug!(
                "Emitting SelectionOwnerChanged for {:?} with mimes: {:?}",
                session_handle_owned,
                mimes_val
            );
            if let Err(e) =
                Self::selection_owner_changed(&emitter, &session_handle_owned, options).await
            {
                tracing::error!("Failed to emit SelectionOwnerChanged: {}", e);
            } else {
                tracing::debug!("Successfully emitted SelectionOwnerChanged");
            }
        } else {
            tracing::error!("Failed to create SignalEmitter");
        }
        Ok(())
    }

    async fn set_selection(
        &self,
        session_handle: ObjectPath<'_>,
        options: HashMap<&str, Value<'_>>,
    ) -> fdo::Result<()> {
        tracing::debug!("SetSelection called for session: {:?}", session_handle);
        let mut mimes = Vec::new();
        if let Some(Value::Array(arr)) = options.get("mime_types") {
            for i in 0..arr.len() {
                if let Ok(Some(Value::Str(s))) = arr.get::<Value<'_>>(i) {
                    mimes.push(s.as_str().into());
                }
            }
        }

        let (tx, rx) = channel();
        let _ = self.proxy.sender.send(Box::new(move || {
            let res = gtk_backend::claim_selection(mimes);
            let _ = tx.send(res);
        }));

        let mut request_rx = rx
            .await
            .map_err(|_| fdo::Error::Failed("UI thread dropped channel".into()))?
            .map_err(|e| fdo::Error::Failed(format!("Failed to claim selection: {}", e)))?;

        let pending_transfers_clone = self.pending_transfers.clone();
        let conn_clone = self.connection.clone();
        let session_handle_owned = session_handle.into_owned();

        tokio::spawn(async move {
            // This task handles the host requesting data *from* the sandbox.
            // It dies when `request_rx` is dropped, which happens when the host copies
            // something else and our ContentProvider is destroyed.
            while let Some((mime, fd_sender)) = request_rx.recv().await {
                let Ok(emitter) = SignalEmitter::new(&conn_clone, crate::core::DBUS_PATH) else {
                    return;
                };

                static SERIAL: AtomicU32 = AtomicU32::new(1);
                let serial = SERIAL.fetch_add(1, Ordering::SeqCst);

                pending_transfers_clone
                    .lock()
                    .insert(serial, TransferRequest { fd_sender });

                if let Err(e) =
                    Self::selection_transfer(&emitter, &session_handle_owned, &mime, serial).await
                {
                    tracing::error!("Failed to emit SelectionTransfer: {}", e);
                    pending_transfers_clone.lock().remove(&serial);
                } else {
                    let pending = pending_transfers_clone.clone();
                    tokio::spawn(async move {
                        tokio::time::sleep(Duration::from_secs(10)).await;
                        if pending.lock().remove(&serial).is_some() {
                            tracing::warn!("Clipboard transfer request {} timed out", serial);
                        }
                    });
                }
            }
        });

        Ok(())
    }

    async fn selection_write(
        &self,
        session_handle: ObjectPath<'_>,
        serial: u32,
    ) -> fdo::Result<Fd<'_>> {
        tracing::debug!(
            "SelectionWrite called for session: {:?} serial: {}",
            session_handle,
            serial
        );
        let transfer = self
            .pending_transfers
            .lock()
            .remove(&serial)
            .ok_or_else(|| fdo::Error::InvalidArgs(format!("Invalid serial {}", serial)))?;

        let (read_fd, write_fd) = rustix::pipe::pipe()
            .map_err(|e| fdo::Error::Failed(format!("Failed to create pipe: {}", e)))?;

        // Send the read end to the backend provider
        if transfer.fd_sender.send(read_fd).is_err() {
            return Err(fdo::Error::Failed("Backend is no longer listening".into()));
        }

        // Return the write end to the DBus caller so they can stream data directly
        // to the GTK backend via the pipe.
        Ok(Fd::from(write_fd))
    }

    async fn selection_write_done(
        &self,
        session_handle: ObjectPath<'_>,
        serial: u32,
        success: bool,
    ) -> fdo::Result<()> {
        tracing::debug!(
            "SelectionWriteDone called for session: {:?} serial: {} success: {}",
            session_handle,
            serial,
            success
        );
        self.pending_transfers.lock().remove(&serial);
        Ok(())
    }

    async fn selection_read(
        &self,
        session_handle: ObjectPath<'_>,
        mime_type: String,
    ) -> fdo::Result<Fd<'_>> {
        tracing::debug!(
            "SelectionRead called for session: {:?} mime_type: {}",
            session_handle,
            mime_type
        );

        let (read_fd, write_fd) = rustix::pipe::pipe()
            .map_err(|e| fdo::Error::Failed(format!("Failed to create pipe: {}", e)))?;

        let _ = self.proxy.sender.send(Box::new(move || {
            if let Err(e) = gtk_backend::read_selection(mime_type, write_fd) {
                tracing::error!("Failed to read selection: {}", e);
            }
        }));

        Ok(Fd::from(read_fd))
    }

    #[zbus(signal)]
    async fn selection_owner_changed(
        ctxt: &SignalEmitter<'_>,
        session_handle: &ObjectPath<'_>,
        options: HashMap<&str, &Value<'_>>,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn selection_transfer(
        ctxt: &SignalEmitter<'_>,
        session_handle: &ObjectPath<'_>,
        mime_type: &str,
        serial: u32,
    ) -> zbus::Result<()>;

    #[zbus(property)]
    fn version(&self) -> u32 {
        2
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*, crate::core::session_manager::SessionManager, gtk4::glib::MainContext,
        tokio::sync::mpsc::unbounded_channel, zbus::zvariant::OwnedObjectPath,
    };

    fn dummy_proxy() -> UiProxy {
        let (sender, _receiver) = unbounded_channel();
        UiProxy {
            context: MainContext::default(),
            sender,
        }
    }

    #[tokio::test]
    async fn test_clipboard_version() -> Result<(), Box<dyn std::error::Error>> {
        if std::env::var("RUN_DBUS_TESTS").is_err() {
            return Ok(());
        }
        let conn = Connection::session().await?;
        let sm = SessionManager::new(conn.clone(), 10);
        let portal = ClipboardPortal::new(conn, dummy_proxy(), sm);
        assert_eq!(portal.version(), 2);
        Ok(())
    }

    #[tokio::test]
    async fn test_selection_write_invalid_serial() -> Result<(), Box<dyn std::error::Error>> {
        if std::env::var("RUN_DBUS_TESTS").is_err() {
            return Ok(());
        }
        let conn = Connection::session().await?;
        let sm = SessionManager::new(conn.clone(), 10);
        let portal = ClipboardPortal::new(conn, dummy_proxy(), sm);

        let path = ObjectPath::try_from("/org/freedesktop/portal/desktop/session/1/1").unwrap();
        let res = portal.selection_write(path.clone(), 9999).await;

        assert!(res.is_err());
        assert!(matches!(res.unwrap_err(), fdo::Error::InvalidArgs(_)));
        Ok(())
    }
}
