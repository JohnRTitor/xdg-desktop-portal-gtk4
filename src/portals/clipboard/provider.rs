use {
    gtk4::{gdk, gio, glib},
    std::{cell::RefCell, os::fd::OwnedFd},
    tokio::sync::mpsc::Sender,
};

mod imp {
    use {super::*, gtk4::gdk::subclass::prelude::*};

    pub type FdRequestSender = Sender<(String, tokio::sync::oneshot::Sender<OwnedFd>)>;

    #[derive(Default)]
    pub struct PortalContentProvider {
        pub mimes: RefCell<Vec<String>>,
        pub request_tx: RefCell<Option<FdRequestSender>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PortalContentProvider {
        const NAME: &'static str = "PortalContentProvider";
        type Type = super::PortalContentProvider;
        type ParentType = gdk::ContentProvider;
    }

    impl ObjectImpl for PortalContentProvider {}

    impl ContentProviderImpl for PortalContentProvider {
        fn formats(&self) -> gdk::ContentFormats {
            let mut builder = gdk::ContentFormatsBuilder::new();
            for mime in self.mimes.borrow().iter() {
                builder = builder.add_mime_type(mime);
            }
            builder.build()
        }

        fn storable_formats(&self) -> gdk::ContentFormats {
            self.formats()
        }

        fn write_mime_type_future(
            &self,
            mime_type: &str,
            stream: &gio::OutputStream,
            _io_priority: glib::Priority,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), glib::Error>> + 'static>>
        {
            let mime_type = mime_type.to_string();
            let out_stream = stream.clone();

            // We clone the sender so we can use it in the future
            let request_tx = self.request_tx.borrow().clone();

            Box::pin(async move {
                let Some(tx) = request_tx else {
                    return Err(glib::Error::new(
                        gio::IOErrorEnum::Failed,
                        "No request channel",
                    ));
                };

                // Create a channel for receiving the file descriptor
                let (fd_tx, fd_rx) = tokio::sync::oneshot::channel();

                // Send the request for a file descriptor
                if tx.send((mime_type.to_string(), fd_tx)).await.is_err() {
                    return Err(glib::Error::new(
                        gio::IOErrorEnum::Failed,
                        "Failed to send request for file descriptor",
                    ));
                }

                // Wait for the file descriptor
                let fd = fd_rx.await.map_err(|_| {
                    glib::Error::new(
                        gio::IOErrorEnum::Failed,
                        "Failed to receive file descriptor",
                    )
                })?;

                // Wrap the file descriptor in a File and then an InputStream
                let file = std::fs::File::from(fd);
                let in_stream = gio::ReadInputStream::new(file);

                // Asynchronously copy data from the provider to our output stream
                use gio::prelude::*;
                out_stream
                    .splice_future(
                        &in_stream,
                        gio::OutputStreamSpliceFlags::CLOSE_SOURCE
                            | gio::OutputStreamSpliceFlags::CLOSE_TARGET,
                        glib::Priority::default(),
                    )
                    .await
                    .map_err(|e| glib::Error::new(gio::IOErrorEnum::Failed, &e.to_string()))?;

                Ok(())
            })
        }
    }
}

glib::wrapper! {
    pub struct PortalContentProvider(ObjectSubclass<imp::PortalContentProvider>)
        @extends gdk::ContentProvider;
}

impl PortalContentProvider {
    pub fn new(mimes: Vec<String>, request_tx: imp::FdRequestSender) -> Self {
        let obj: Self = glib::Object::new();
        let imp = gtk4::subclass::prelude::ObjectSubclassIsExt::imp(&obj);
        *imp.mimes.borrow_mut() = mimes;
        *imp.request_tx.borrow_mut() = Some(request_tx);
        obj
    }
}
