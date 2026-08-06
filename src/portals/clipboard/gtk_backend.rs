use {
    gtk4::{gdk, gio, glib, prelude::*},
    std::{cell::RefCell, os::fd::OwnedFd},
    tokio::sync::{
        mpsc::{Receiver, channel},
        oneshot::Sender as OneshotSender,
    },
};

const CHANNEL_BUFFER_SIZE: usize = 5;

#[derive(Debug, thiserror::Error)]
pub enum ClipboardError {
    #[error("Clipboard not available")]
    NotAvailable,
    #[error("GTK clipboard error: {0}")]
    Gtk(String),
}

pub struct GtkClipboardBackend {
    clipboard: gdk::Clipboard,
    formats_sender: tokio::sync::broadcast::Sender<Vec<String>>,
}

thread_local! {
    static BACKEND: RefCell<Option<GtkClipboardBackend>> = const { RefCell::new(None) };
}

pub fn get_backend<F, R>(f: F) -> Result<R, ClipboardError>
where
    F: FnOnce(&mut GtkClipboardBackend) -> R,
{
    BACKEND.with(|b| {
        let mut b_ref = b.borrow_mut();
        let backend = match &mut *b_ref {
            Some(b) => b,
            None => b_ref.insert(GtkClipboardBackend::new()?),
        };
        Ok(f(backend))
    })
}

impl GtkClipboardBackend {
    pub fn new() -> Result<Self, ClipboardError> {
        let display = gdk::Display::default().ok_or(ClipboardError::NotAvailable)?;
        let clipboard = display.clipboard();

        let (formats_tx, _) = tokio::sync::broadcast::channel(CHANNEL_BUFFER_SIZE);
        let formats_tx_clone = formats_tx.clone();

        clipboard.connect_formats_notify(move |cb| {
            let formats = cb.formats();
            let mimes: Vec<String> = formats.mime_types().into_iter().map(String::from).collect();
            let _ = formats_tx_clone.send(mimes);
        });

        Ok(Self {
            clipboard,
            formats_sender: formats_tx,
        })
    }
}

pub fn claim_selection(
    mimes: Vec<String>,
) -> Result<Receiver<(String, OneshotSender<OwnedFd>)>, ClipboardError> {
    get_backend(|backend| {
        let (request_tx, request_rx) = channel(10);
        let provider =
            crate::portals::clipboard::provider::PortalContentProvider::new(mimes, request_tx);

        if backend.clipboard.set_content(Some(&provider)).is_err() {
            return Err(ClipboardError::Gtk(
                "Failed to set clipboard content provider".into(),
            ));
        }
        Ok(request_rx)
    })?
}

pub fn read_selection(mime: String, fd: OwnedFd) -> Result<(), ClipboardError> {
    get_backend(|backend| {
        let clipboard = backend.clipboard.clone();
        glib::MainContext::default().spawn_local(async move {
            match clipboard
                .read_future(&[&mime], glib::Priority::default())
                .await
            {
                Ok((in_stream, _)) => {
                    let file = std::fs::File::from(fd);
                    let out_stream = gio::WriteOutputStream::new(file);
                    match out_stream
                        .splice_future(
                            &in_stream,
                            gio::OutputStreamSpliceFlags::CLOSE_SOURCE
                                | gio::OutputStreamSpliceFlags::CLOSE_TARGET,
                            glib::Priority::default(),
                        )
                        .await
                    {
                        Ok(_) => tracing::debug!("splice_future succeeded"),
                        Err(e) => tracing::error!("splice_future failed: {}", e),
                    }
                }
                Err(e) => tracing::error!("clipboard.read_future failed: {}", e),
            }
        });
    })?;
    Ok(())
}

pub fn subscribe_changes() -> Result<tokio::sync::broadcast::Receiver<Vec<String>>, ClipboardError>
{
    get_backend(|backend| backend.formats_sender.subscribe())
}

pub fn current_formats() -> Result<Vec<String>, ClipboardError> {
    get_backend(|backend| {
        backend
            .clipboard
            .formats()
            .mime_types()
            .into_iter()
            .map(String::from)
            .collect()
    })
}
