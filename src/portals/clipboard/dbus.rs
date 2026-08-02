use {
    crate::portals::clipboard::gtk_backend,
    async_channel::Sender,
    std::{
        collections::HashMap,
        os::fd::OwnedFd,
        sync::{
            atomic::{AtomicU32, Ordering},
            Arc, Mutex,
        },
    },
    zbus::{
        fdo, interface, object_server::SignalEmitter,
        zvariant::{Fd, ObjectPath, Value},
        Connection,
    },
};

struct TransferRequest {
    fd_sender: Sender<OwnedFd>,
}

pub struct ClipboardPortal {
    active_sessions: Arc<Mutex<Vec<ObjectPath<'static>>>>,
    pending_transfers: Arc<Mutex<HashMap<u32, TransferRequest>>>,
    connection: Connection,
}

impl ClipboardPortal {
    pub fn new(connection: Connection) -> Self {
        let pending_transfers = Arc::new(Mutex::new(HashMap::new()));
        let active_sessions = Arc::new(Mutex::new(Vec::new()));

        match gtk_backend::subscribe_changes() {
            Ok(formats_rx) => {
                let conn_clone = connection.clone();
                let sessions_clone = active_sessions.clone();

                gtk4::glib::MainContext::default().spawn_local(async move {
                    while let Ok(mimes) = formats_rx.recv().await {
                let emitter = match SignalEmitter::new(&conn_clone, "/org/freedesktop/portal/desktop") {
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

                    let sessions: Vec<_> = sessions_clone.lock().unwrap_or_else(|e| e.into_inner()).clone();
                    for session in sessions {
                        let _ = Self::selection_owner_changed(&emitter, &session, options.clone()).await;
                    }
                }
            });
            }
            Err(e) => {
                tracing::warn!("Clipboard portal backend unavailable: {}", e);
            }
        }

        Self {
            active_sessions,
            pending_transfers,
            connection,
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
        let mut sessions = self.active_sessions.lock().unwrap_or_else(|e| e.into_inner());
        if !sessions.contains(&session_handle.to_owned()) {
            sessions.push(session_handle.into_owned());
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

        let request_rx = gtk_backend::claim_selection(mimes)
            .map_err(|e| fdo::Error::Failed(format!("Failed to claim selection: {}", e)))?;

        let pending_transfers = self.pending_transfers.clone();
        let conn_clone = self.connection.clone();
        let session_handle_owned = session_handle.into_owned();

        gtk4::glib::MainContext::default().spawn_local(async move {
            // This task dies when `request_rx` is dropped, which happens when the host copies
            // something else and our ContentProvider is destroyed.
            while let Ok((mime, fd_sender)) = request_rx.recv().await {
                let emitter = match SignalEmitter::new(&conn_clone, "/org/freedesktop/portal/desktop") {
                    Ok(e) => e,
                    Err(_) => return,
                };
                
                static SERIAL: AtomicU32 = AtomicU32::new(1);
                let serial = SERIAL.fetch_add(1, Ordering::SeqCst);

                pending_transfers.lock().unwrap_or_else(|e| e.into_inner()).insert(
                    serial,
                    TransferRequest {
                        fd_sender,
                    },
                );

                if let Err(e) = Self::selection_transfer(&emitter, &session_handle_owned, &mime, serial).await {
                    tracing::error!("Failed to emit SelectionTransfer: {}", e);
                    pending_transfers.lock().unwrap_or_else(|e| e.into_inner()).remove(&serial);
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
        tracing::debug!("SelectionWrite called for session: {:?} serial: {}", session_handle, serial);
        let transfer = self
            .pending_transfers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&serial)
            .ok_or_else(|| fdo::Error::InvalidArgs(format!("Invalid serial {}", serial)))?;

        let (read_fd, write_fd) = rustix::pipe::pipe().map_err(|e| {
            fdo::Error::Failed(format!("Failed to create pipe: {}", e))
        })?;

        // Send the read end to the backend provider
        if transfer.fd_sender.try_send(read_fd).is_err() {
            return Err(fdo::Error::Failed("Backend is no longer listening".into()));
        }

        // Return the write end to the DBus caller
        Ok(Fd::from(write_fd))
    }

    async fn selection_write_done(
        &self,
        session_handle: ObjectPath<'_>,
        serial: u32,
        success: bool,
    ) -> fdo::Result<()> {
        tracing::debug!("SelectionWriteDone called for session: {:?} serial: {} success: {}", session_handle, serial, success);
        self.pending_transfers.lock().unwrap_or_else(|e| e.into_inner()).remove(&serial);
        Ok(())
    }

    async fn selection_read(
        &self,
        session_handle: ObjectPath<'_>,
        mime_type: String,
    ) -> fdo::Result<Fd<'_>> {
        tracing::debug!("SelectionRead called for session: {:?} mime_type: {}", session_handle, mime_type);
        
        let (read_fd, write_fd) = rustix::pipe::pipe().map_err(|e| {
            fdo::Error::Failed(format!("Failed to create pipe: {}", e))
        })?;

        if let Err(e) = gtk_backend::read_selection(mime_type, write_fd) {
            return Err(fdo::Error::Failed(format!("Failed to read selection: {}", e)));
        }

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
