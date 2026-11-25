use {
    gtk4::{gdk, gio, glib, subclass::prelude::ObjectSubclassIsExt},
    std::{cell::RefCell, fs::File, future::Future, os::fd::OwnedFd, pin::Pin},
    tokio::sync::{
        mpsc,
        oneshot::{self, Sender as OneshotSender},
    },
};

use super::gtk_backend::MimeList;

mod imp {
    use {super::*, gtk4::gdk::subclass::prelude::*};

    pub type FdRequestSender = mpsc::Sender<(Box<str>, OneshotSender<OwnedFd>)>;

    #[derive(Default)]
    pub struct PortalContentProvider {
        pub mimes: RefCell<MimeList>,
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
                builder = builder.add_mime_type(mime.as_ref());
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
        ) -> Pin<Box<dyn Future<Output = Result<(), glib::Error>> + 'static>> {
            let mime_type: Box<str> = mime_type.into();
            let out_stream = stream.clone();

            // The future needs an owned sender, while the provider keeps its own.
            let request_tx = self.request_tx.borrow().clone();

            Box::pin(async move {
                let Some(tx) = request_tx else {
                    return Err(glib::Error::new(
                        gio::IOErrorEnum::Failed,
                        "No request channel",
                    ));
                };

                let (fd_tx, fd_rx) = oneshot::channel();

                if tx.send((mime_type, fd_tx)).await.is_err() {
                    return Err(glib::Error::new(
                        gio::IOErrorEnum::Failed,
                        "Failed to send request for file descriptor",
                    ));
                }

                let fd = fd_rx.await.map_err(|_| {
                    glib::Error::new(
                        gio::IOErrorEnum::Failed,
                        "Failed to receive file descriptor",
                    )
                })?;

                let file = File::from(fd);
                let in_stream = gio::ReadInputStream::new(file);

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
    pub fn new(mimes: MimeList, request_tx: imp::FdRequestSender) -> Self {
        let obj: Self = glib::Object::new();
        let imp = ObjectSubclassIsExt::imp(&obj);
        *imp.mimes.borrow_mut() = mimes;
        *imp.request_tx.borrow_mut() = Some(request_tx);
        obj
    }
}
