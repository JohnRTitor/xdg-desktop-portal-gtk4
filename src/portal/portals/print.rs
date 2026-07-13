use {
    crate::{
        gui::{print::PrintUi, UiProxy},
        portal::{request::run_request, response::Response},
    },
    error_reporter::Report,
    zbus::{
        interface,
        zvariant::{DeserializeDict, Fd, OwnedObjectPath, SerializeDict, Type, Value},
        ObjectServer,
    },
    std::collections::HashMap,
};

pub struct Print {
    proxy: UiProxy,
}

impl Print {
    pub fn new(proxy: &UiProxy) -> Self {
        Self {
            proxy: proxy.clone(),
        }
    }
}

#[derive(DeserializeDict, Type, Debug, Default)]
#[zvariant(signature = "dict")]
struct PreparePrintOptions<'a> {
    modal: Option<bool>,
    token: Option<String>,
    #[zvariant(rename = "accept-format")]
    accept_format: Option<String>,
    #[zvariant(rename = "accept-media")]
    accept_media: Option<String>,
    #[zvariant(rename = "accept-papers")]
    accept_papers: Option<String>,
    settings: Option<HashMap<String, Value<'a>>>,
    page_setup: Option<HashMap<String, Value<'a>>>,
}

#[derive(SerializeDict, Type, Debug, Default)]
#[zvariant(signature = "dict")]
struct PreparePrintResults<'a> {
    settings: HashMap<String, Value<'a>>,
    page_setup: HashMap<String, Value<'a>>,
    token: u32,
}

#[derive(DeserializeDict, Type, Debug, Default)]
#[zvariant(signature = "dict")]
struct PrintOptions<'a> {
    modal: Option<bool>,
    token: Option<u32>,
    #[zvariant(rename = "accept-format")]
    accept_format: Option<String>,
    settings: Option<HashMap<String, Value<'a>>>,
    page_setup: Option<HashMap<String, Value<'a>>>,
}

#[derive(SerializeDict, Type, Debug, Default)]
#[zvariant(signature = "dict")]
struct PrintResults {}

impl Print {
    async fn prepare_print_impl(
        &self,
        app_id: String,
        parent_window: String,
        title: String,
        _settings: HashMap<String, Value<'_>>,
        _page_setup: HashMap<String, Value<'_>>,
        _options: PreparePrintOptions<'_>,
    ) -> Response<PreparePrintResults<'static>> {
        let res = PrintUi {
            app_id,
            parent_window,
            title,
        }
        .run(&self.proxy)
        .await;

        match res {
            Ok(_) => Response::success(PreparePrintResults {
                settings: HashMap::new(),
                page_setup: HashMap::new(),
                token: 0,
            }),
            Err(e) => {
                log::error!("PreparePrint failed: {}", Report::new(e));
                Response::cancelled()
            }
        }
    }

    async fn print_impl(
        &self,
        _app_id: String,
        _parent_window: String,
        _title: String,
        _fd: Fd<'_>,
        _options: PrintOptions<'_>,
    ) -> Response<PrintResults> {
        // Dummy implementation since actual printing requires complex GTK CUPS backends
        log::info!("Print method called, returning success.");
        Response::success(PrintResults::default())
    }
}

#[interface(name = "org.freedesktop.impl.portal.Print")]
impl Print {
    async fn prepare_print(
        &self,
        handle: OwnedObjectPath,
        app_id: String,
        parent_window: String,
        title: String,
        settings: HashMap<String, Value<'_>>,
        page_setup: HashMap<String, Value<'_>>,
        options: PreparePrintOptions<'_>,
        #[zbus(object_server)] server: &ObjectServer,
    ) -> Response<PreparePrintResults<'static>> {
        run_request(
            server,
            handle,
            self.prepare_print_impl(app_id, parent_window, title, settings, page_setup, options),
        )
        .await
    }

    async fn print(
        &self,
        handle: OwnedObjectPath,
        app_id: String,
        parent_window: String,
        title: String,
        fd: Fd<'_>,
        options: PrintOptions<'_>,
        #[zbus(object_server)] server: &ObjectServer,
    ) -> Response<PrintResults> {
        run_request(
            server,
            handle,
            self.print_impl(app_id, parent_window, title, fd, options),
        )
        .await
    }
}
