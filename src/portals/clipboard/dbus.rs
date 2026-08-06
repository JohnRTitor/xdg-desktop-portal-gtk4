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
    parking_lot::Mutex,
    std::{
        collections::HashMap,
        os::fd::OwnedFd,
        sync::{
            Arc,
            atomic::{AtomicU32, Ordering},
        },
    },
    zbus::{
        Connection, fdo, interface,
        object_server::SignalEmitter,
        zvariant::{Fd, ObjectPath, Value},
    },
};

struct TransferRequest {
    fd_sender: tokio::sync::oneshot::Sender<OwnedFd>,
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
}

impl ClipboardPortal {
    pub fn new(connection: Connection, proxy: UiProxy) -> Self {
        let pending_transfers = Arc::new(Mutex::new(HashMap::new()));
        let active_sessions = Arc::new(Mutex::new(Vec::new()));

        let conn_clone = connection.clone();
        let sessions_clone = active_sessions.clone();

        let (tx, rx) = tokio::sync::oneshot::channel();

        // Run GTK-specific initialization on the main thread and pipe the event stream back to Tokio
        let _ = proxy.sender.send(Box::new(move || {
            gtk4::glib::MainContext::default().spawn_local(async move {
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
            if let Ok(mut formats_rx) = rx.await {
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
                        let _ = Self::selection_owner_changed(&emitter, &session, options.clone())
                            .await;
                    }
                }
            }
        });

        Self {
            active_sessions,
            pending_transfers,
            connection,
            proxy,
        }
    }
}

#[interface(name = "org.freedesktop.impl.portal.Clipboard")]
impl ClipboardPortal {
    async fn request_clipboard(
        &self,
        session_handle: ObjectPath<'_>,
        _options: HashMap<&str, Value<'_>>,
    ) -> fdo::Result<()> {
        tracing::debug!("RequestClipboard called for session: {:?}", session_handle);
        let session_handle_owned = session_handle.into_owned();
        {
            let mut sessions = self.active_sessions.lock();
            if !sessions.contains(&session_handle_owned) {
                sessions.push(session_handle_owned.clone());
            }
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
                    mimes.push(s.as_str().to_string());
                }
            }
        }

        let (tx, rx) = tokio::sync::oneshot::channel();
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
                let emitter = match SignalEmitter::new(&conn_clone, crate::core::DBUS_PATH) {
                    Ok(e) => e,
                    Err(_) => return,
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
                    pending_transfers_clone
                        .lock()
                        .remove(&serial);
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
        self.pending_transfers
            .lock()
            .remove(&serial);
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
