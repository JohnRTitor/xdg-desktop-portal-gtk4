use {
    async_channel::Sender,
    gtk4::{gdk, gio, glib},
    std::cell::RefCell,
    std::os::fd::OwnedFd,
};

mod imp {
    use super::*;
    use gtk4::gdk::subclass::prelude::*;

    #[derive(Default)]
    pub struct PortalContentProvider {
        pub mimes: RefCell<Vec<String>>,
        pub request_tx: RefCell<Option<Sender<(String, Sender<OwnedFd>)>>>,
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
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), glib::Error>> + 'static>> {
            let mime_type = mime_type.to_string();
            let stream = stream.clone();
            
            // We clone the sender so we can use it in the future
            let request_tx = self.request_tx.borrow().clone();

            Box::pin(async move {
                if let Some(tx) = request_tx {
                    // Create a channel for receiving the file descriptor
                    let (fd_tx, fd_rx) = async_channel::bounded(1);
                    
                    // Send the request to the D-Bus side
                    if tx.send((mime_type, fd_tx)).await.is_err() {
                        return Err(glib::Error::new(
                            gio::IOErrorEnum::Failed,
                            "Failed to notify portal of transfer request",
                        ));
                    }

                    // Wait for the file descriptor to be provided by D-Bus SelectionWrite
                    let fd = match fd_rx.recv().await {
                        Ok(fd) => fd,
                        Err(_) => {
                            return Err(glib::Error::new(
                                gio::IOErrorEnum::Failed,
                                "Portal failed to provide file descriptor",
                            ));
                        }
                    };

                    // Splice data from the pipe to the GTK stream
                    use gio::prelude::*;
                    
                    let file = std::fs::File::from(fd);
                    let in_stream = gio::ReadInputStream::new(file);
                    
                    stream.splice_future(
                        &in_stream,
                        gio::OutputStreamSpliceFlags::CLOSE_SOURCE | gio::OutputStreamSpliceFlags::CLOSE_TARGET,
                        glib::Priority::default(),
                    ).await.map_err(|e| glib::Error::new(gio::IOErrorEnum::Failed, &e.to_string()))?;

                    Ok(())
                } else {
                    Err(glib::Error::new(gio::IOErrorEnum::Failed, "No request channel"))
                }
            })
        }
    }
}

glib::wrapper! {
    pub struct PortalContentProvider(ObjectSubclass<imp::PortalContentProvider>)
        @extends gdk::ContentProvider;
}

impl PortalContentProvider {
    pub fn new(mimes: Vec<String>, request_tx: Sender<(String, Sender<OwnedFd>)>) -> Self {
        let obj: Self = glib::Object::new();
        let imp = gtk4::subclass::prelude::ObjectSubclassIsExt::imp(&obj);
        *imp.mimes.borrow_mut() = mimes;
        *imp.request_tx.borrow_mut() = Some(request_tx);
        obj
    }
}
