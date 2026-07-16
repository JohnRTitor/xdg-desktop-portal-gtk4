use {
    super::gui::{ExecutePrintUi, PrintUi},
    crate::{
        core::{request::run_request, response::Response},
        gui::UiProxy,
    },
    std::collections::HashMap,
    zbus::{
        ObjectServer, interface,
        zvariant::{DeserializeDict, Fd, OwnedObjectPath, OwnedValue, SerializeDict, Type, Value},
    },
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
    pub activation_token: Option<String>,
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
    pub activation_token: Option<String>,
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
        options: PreparePrintOptions,
    ) -> Response<PreparePrintResults> {
        let res = PrintUi {
            app_id,
            parent_window,
            activation_token: options.activation_token.clone(),
            title,
        }
        .run(&self.proxy)
        .await;

        match res {
            Ok(result) => Response::success(PreparePrintResults {
                settings: result.settings,
                page_setup: result.page_setup,
                token: result.token,
                supported_output_file_formats: Some(vec![
                    "pdf".to_string(),
                    "ps".to_string(),
                    "svg".to_string(),
                ]),
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

        // The file descriptor provided by the portal daemon must be passed to the print system.
        // The D-Bus library (zbus) internally dup's the fd during deserialization.
        // Because we process this on the GTK main thread synchronously (awaiting the channel),
        // it is safe to extract the raw_fd here and pass it down.
        let raw_fd = fd.as_raw_fd();

        let res = ExecutePrintUi { token, fd: raw_fd }.run(&self.proxy).await;

        match res {
            Ok(_) => Response::success(PrintResults::default()),
            Err(e) => {
                log::error!("Print dispatch failed: {}", anyhow::Error::new(e));
                Response::cancelled()
            }
        }
    }
}

/// The D-Bus interface implementation for `org.freedesktop.impl.portal.Print`.
///
/// This portal implements a two-step printing process:
/// 1. `PreparePrint`: Displays the print dialog, allowing the user to select a printer and settings.
///    It returns a unique token to the frontend.
/// 2. `Print`: The frontend provides the document data via a file descriptor, along with the token.
///    The portal matches the token to the cached print settings and submits the job to CUPS.
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

#[cfg(test)]
mod tests {
    use {super::*, zbus::zvariant::Type};

    #[test]
    fn test_prepare_print_options_signature() {
        assert_eq!(PreparePrintOptions::SIGNATURE, "a{sv}");
    }

    #[test]
    fn test_prepare_print_results_signature() {
        assert_eq!(PreparePrintResults::SIGNATURE, "a{sv}");
    }

    #[test]
    fn test_print_options_signature() {
        assert_eq!(PrintOptions::SIGNATURE, "a{sv}");
    }

    #[test]
    fn test_print_results_signature() {
        assert_eq!(PrintResults::SIGNATURE, "a{sv}");
    }
}
