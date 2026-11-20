/// Flatten a portal options dict into the string map the print UI consumes,
/// keeping only the string values that GTK can be fed through a KeyFile.
fn flatten_dict(dict: HashMap<String, Value<'_>>) -> HashMap<String, String> {
    dict.into_iter()
        .filter_map(|(k, v)| match &v {
            Value::Str(s) => Some((k, s.to_string())),
            _ => None,
        })
        .collect()
}

use {
    super::gui::{ExecutePrintUi, PrintTokenProbe, PrintUi},
    crate::{
        core::{request::run_request, response::Response},
        gui::UiProxy,
    },
    std::{collections::HashMap, os::fd::AsFd},
    zbus::{
        ObjectServer, fdo, interface,
        message::Header,
        zvariant::{DeserializeDict, Fd, OwnedObjectPath, OwnedValue, SerializeDict, Type, Value},
    },
};

/// D-Bus interface wrapper for the Print portal.
///
/// This struct holds a proxy to the GTK main loop. The GTK printing implementation
/// handles maintaining the state between `PreparePrint` and `Print` via the token.
pub struct Print {
    proxy: UiProxy,
    session_manager: crate::core::session_manager::SessionManager,
}

impl Print {
    pub fn new(
        proxy: &UiProxy,
        session_manager: crate::core::session_manager::SessionManager,
    ) -> Self {
        Self {
            proxy: proxy.clone(),
            session_manager,
        }
    }
}

#[derive(DeserializeDict, Type, Debug, Default)]
#[zvariant(signature = "dict")]
#[allow(dead_code)]
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
#[allow(dead_code)]
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
        settings: HashMap<String, Value<'_>>,
        page_setup: HashMap<String, Value<'_>>,
        options: PreparePrintOptions,
    ) -> Response<PreparePrintResults> {
        let res = PrintUi {
            app_id,
            parent_window,
            activation_token: options.activation_token.clone(),
            title,
            // These used to be discarded, so an application that requested a
            // particular printer, paper size or orientation got a dialog that
            // ignored all of it.
            settings: flatten_dict(settings),
            page_setup: flatten_dict(page_setup),
        }
        .run(&self.proxy)
        .await;

        match res {
            Ok(result) => Response::success(PreparePrintResults {
                settings: result.settings,
                page_setup: result.page_setup,
                token: result.token,
                supported_output_file_formats: Some(vec!["pdf".into(), "ps".into(), "svg".into()]),
                has_current_page: Some(true),
                has_selected_pages: Some(true),
            }),
            Err(e) => {
                tracing::error!(error = %e, "PreparePrint failed");
                Response::from_ui_error(e)
            }
        }
    }

    async fn print_impl(
        &self,
        app_id: String,
        parent_window: String,
        title: String,
        fd: Fd<'_>,
        options: PrintOptions,
    ) -> Response<PrintResults> {
        let mut token = options.token.unwrap_or(0);

        // A token from `PreparePrint` authorises a job cached there. Callers that
        // skip that step (`GtkPrintDialog::print_file()` passes no setup, so GTK
        // sends `token = 0`) expect `Print` to present the dialog itself, the way
        // the GTK3 and GNOME backends do. The probe does not consume a token; a
        // foreign token is still refused by `claim_token` when the job runs.
        // Token 0 skips the probe entirely: it is the "no token" sentinel, not
        // something that could name a prepared job. A real probe failure is an
        // error, not evidence that the caller skipped `PreparePrint`.
        let exists = if token == 0 {
            false
        } else {
            match (PrintTokenProbe { token }).run(&self.proxy).await {
                Ok(exists) => exists,
                Err(e) => {
                    tracing::error!(error = %e, "Print token probe failed");
                    return Response::from_ui_error(e);
                }
            }
        };

        if !exists {
            match (PrintUi {
                app_id: app_id.clone(),
                parent_window,
                activation_token: options.activation_token,
                title,
                settings: HashMap::new(),
                page_setup: HashMap::new(),
            })
            .run(&self.proxy)
            .await
            {
                Ok(result) => token = result.token,
                Err(e) => {
                    tracing::error!(error = %e, "Print dialog failed");
                    return Response::from_ui_error(e);
                }
            }
        }

        // zvariant borrows the received fd from the D-Bus message, which zbus
        // closes as soon as this handler returns. GTK's backend reads the fd
        // asynchronously after `send()`, so hand the job its own descriptor and
        // let `ExecutePrintUi` close it once the job is finished. GTK does not
        // close the descriptor it is given. Use a CLOEXEC duplicate, like the
        // fd zbus received, so a concurrently spawned helper cannot inherit it
        // during a long print job.
        let source_fd = match rustix::io::fcntl_dupfd_cloexec(fd.as_fd(), 0) {
            Ok(fd) => fd,
            Err(e) => {
                tracing::error!(error = %e, "Failed to duplicate print source fd");
                return Response::other();
            }
        };

        let res = ExecutePrintUi {
            token,
            app_id,
            fd: source_fd,
        }
        .run(&self.proxy)
        .await;

        match res {
            Ok(_) => Response::success(PrintResults::default()),
            Err(e) => {
                tracing::error!(error = %e, "Print dispatch failed");
                Response::from_ui_error(e)
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
///
/// A caller that skips `PreparePrint` sends `token = 0` (as `GtkPrintDialog` does when given no
/// setup); `Print` then presents the dialog itself and prints with the resulting job.
#[interface(name = "org.freedesktop.impl.portal.Print")]
impl Print {
    #[allow(clippy::too_many_arguments)]
    #[tracing::instrument(skip_all, fields(app_id = %app_id, handle = %handle.as_str()))]
    async fn prepare_print(
        &self,
        #[zbus(header)] header: Header<'_>,
        handle: OwnedObjectPath,
        app_id: String,
        parent_window: String,
        title: String,
        settings: HashMap<String, Value<'_>>,
        page_setup: HashMap<String, Value<'_>>,
        options: PreparePrintOptions,
        #[zbus(object_server)] server: &ObjectServer,
    ) -> Result<Response<PreparePrintResults>, fdo::Error> {
        let sender = header
            .sender()
            .map(|s| String::from(s.as_str()))
            .ok_or_else(|| fdo::Error::Failed("Missing sender".into()))?;
        Ok(run_request(
            server,
            self.session_manager.clone(),
            &app_id,
            &sender,
            handle,
            self.prepare_print_impl(
                app_id.clone(),
                parent_window,
                title,
                settings,
                page_setup,
                options,
            ),
        )
        .await)
    }

    #[allow(clippy::too_many_arguments)]
    #[tracing::instrument(skip_all, fields(app_id = %app_id, handle = %handle.as_str()))]
    async fn print(
        &self,
        #[zbus(header)] header: Header<'_>,
        handle: OwnedObjectPath,
        app_id: String,
        parent_window: String,
        title: String,
        fd: Fd<'_>,
        options: PrintOptions,
        #[zbus(object_server)] server: &ObjectServer,
    ) -> Result<Response<PrintResults>, fdo::Error> {
        let sender = header
            .sender()
            .map(|s| String::from(s.as_str()))
            .ok_or_else(|| fdo::Error::Failed("Missing sender".into()))?;
        Ok(run_request(
            server,
            self.session_manager.clone(),
            &app_id,
            &sender,
            handle,
            self.print_impl(app_id.clone(), parent_window, title, fd, options),
        )
        .await)
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

    #[test]
    fn test_prepare_print_options_deserialize() {
        use {
            std::collections::HashMap,
            zbus::zvariant::{self, Endian, Value, serialized::Context},
        };

        let mut dict = HashMap::new();
        dict.insert("modal", Value::from(true));
        dict.insert("accept_label", Value::from("Print It"));

        let ctxt = Context::new_dbus(Endian::Little, 0);
        let encoded = zvariant::to_bytes(ctxt, &dict).unwrap();
        let options: PreparePrintOptions = encoded.deserialize().unwrap().0;

        assert_eq!(options.modal, Some(true));
        assert_eq!(options.accept_label.as_deref(), Some("Print It"));
    }

    #[test]
    fn test_print_options_deserialize() {
        use {
            std::collections::HashMap,
            zbus::zvariant::{self, Endian, Value, serialized::Context},
        };

        let mut dict = HashMap::new();
        dict.insert("token", Value::from(12345u32));

        let ctxt = Context::new_dbus(Endian::Little, 0);
        let encoded = zvariant::to_bytes(ctxt, &dict).unwrap();
        let options: PrintOptions = encoded.deserialize().unwrap().0;

        assert_eq!(options.token, Some(12345));
    }

    #[test]
    fn test_prepare_print_results_serialize() {
        use {
            std::collections::HashMap,
            zbus::zvariant::{self, Endian, Value, serialized::Context},
        };

        let results = PreparePrintResults {
            token: 999,
            has_current_page: Some(true),
            ..Default::default()
        };

        let ctxt = Context::new_dbus(Endian::Little, 0);
        let encoded = zvariant::to_bytes(ctxt, &results).unwrap();
        let decoded: HashMap<String, Value> = encoded.deserialize().unwrap().0;

        assert_eq!(
            decoded.get("token").unwrap().try_clone().unwrap(),
            Value::from(999u32)
        );
        assert_eq!(
            decoded
                .get("has_current_page")
                .unwrap()
                .try_clone()
                .unwrap(),
            Value::from(true)
        );
    }
}
