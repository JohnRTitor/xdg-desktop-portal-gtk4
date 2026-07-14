use {
    crate::{
        gui::UiProxy,
        core::{request::run_request, response::Response},
    },
    super::gui::{PrintUi, ExecutePrintUi},
    zbus::{
        interface,
        zvariant::{DeserializeDict, Fd, OwnedObjectPath, SerializeDict, Type, Value, OwnedValue},
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
struct PreparePrintOptions {
    modal: Option<bool>,
    accept_label: Option<String>,
}

#[derive(SerializeDict, Type, Debug, Default)]
#[zvariant(signature = "dict")]
struct PreparePrintResults {
    settings: HashMap<String, OwnedValue>,
    #[zvariant(rename = "page-setup")]
    page_setup: HashMap<String, OwnedValue>,
    token: u32,
    supported_output_file_formats: Option<Vec<String>>,
    has_current_page: Option<bool>,
    has_selected_pages: Option<bool>,
}

#[derive(DeserializeDict, Type, Debug, Default)]
#[zvariant(signature = "dict")]
struct PrintOptions {
    modal: Option<bool>,
    token: Option<u32>,
    supported_output_file_formats: Option<Vec<String>>,
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
        _options: PreparePrintOptions,
    ) -> Response<PreparePrintResults> {
        let res = PrintUi {
            app_id,
            parent_window,
            title,
        }
        .run(&self.proxy)
        .await;

        match res {
            Ok(result) => Response::success(PreparePrintResults {
                settings: result.settings,
                page_setup: result.page_setup,
                token: result.token,
                supported_output_file_formats: Some(vec!["pdf".to_string(), "ps".to_string(), "svg".to_string()]),
                has_current_page: Some(true),
                has_selected_pages: Some(true),
            }),
            Err(e) => {
                log::error!("PreparePrint failed: {}", anyhow::Error::new(e));
                Response::cancelled()
            }
        }
    }

    async fn print_impl(
        &self,
        _app_id: String,
        _parent_window: String,
        _title: String,
        fd: Fd<'_>,
        options: PrintOptions,
    ) -> Response<PrintResults> {
        use std::os::fd::AsRawFd;
        let token = options.token.unwrap_or(0);
        
        // The fd needs to be duplicated if the portal daemon closes it,
        // but since we await the GTK thread synchronously, the raw_fd is valid.
        // Actually, GTK internally dups the FD according to C docs!
        let raw_fd = fd.as_raw_fd();

        let res = ExecutePrintUi {
            token,
            fd: raw_fd,
        }
        .run(&self.proxy)
        .await;

        match res {
            Ok(_) => Response::success(PrintResults::default()),
            Err(e) => {
                log::error!("Print dispatch failed: {}", anyhow::Error::new(e));
                Response::cancelled()
            }
        }
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
        options: PreparePrintOptions,
        #[zbus(object_server)] server: &ObjectServer,
    ) -> Response<PreparePrintResults> {
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
        options: PrintOptions,
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
