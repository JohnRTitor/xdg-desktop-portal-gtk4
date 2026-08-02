use {
    async_channel::{Receiver, Sender},
    gtk4::{gdk, gio, glib, prelude::*},
    std::{
        cell::RefCell,
        os::fd::OwnedFd,
    },
};

#[derive(Debug, thiserror::Error)]
pub enum ClipboardError {
    #[error("Clipboard not available")]
    NotAvailable,
    #[error("GTK clipboard error: {0}")]
    Gtk(String),
}

pub struct GtkClipboardBackend {
    clipboard: gdk::Clipboard,
    formats_receiver: Receiver<Vec<String>>,
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
        if b_ref.is_none() {
            *b_ref = Some(GtkClipboardBackend::new()?);
        }
        Ok(f(b_ref.as_mut().expect("backend is guaranteed to be Some after initialization")))
    })
}

impl GtkClipboardBackend {
    pub fn new() -> Result<Self, ClipboardError> {
        let display = gdk::Display::default().ok_or(ClipboardError::NotAvailable)?;
        let clipboard = display.clipboard();

        let (formats_tx, formats_rx) = async_channel::bounded(5);

        clipboard.connect_formats_notify(move |cb| {
            let formats = cb.formats();
            let mut mimes = Vec::new();
            for mime in formats.mime_types() {
                mimes.push(mime.to_string());
            }
            let _ = formats_tx.try_send(mimes);
        });

        Ok(Self {
            clipboard,
            formats_receiver: formats_rx,
        })
    }
}

pub fn claim_selection(
    mimes: Vec<String>,
) -> Result<Receiver<(String, Sender<OwnedFd>)>, ClipboardError> {
    get_backend(|backend| {
        let (request_tx, request_rx) = async_channel::bounded(10);
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
            let stream_res = clipboard
                .read_future(&[&mime], glib::Priority::default())
                .await;
            if let Ok((in_stream, _)) = stream_res {
                let file = std::fs::File::from(fd);
                let out_stream = gio::WriteOutputStream::new(file);
                let _ = out_stream
                    .splice_future(
                        &in_stream,
                        gio::OutputStreamSpliceFlags::CLOSE_SOURCE
                            | gio::OutputStreamSpliceFlags::CLOSE_TARGET,
                        glib::Priority::default(),
                    )
                    .await;
            }
        });
    })?;
    Ok(())
}

pub fn subscribe_changes() -> Result<Receiver<Vec<String>>, ClipboardError> {
    get_backend(|backend| backend.formats_receiver.clone())
}
