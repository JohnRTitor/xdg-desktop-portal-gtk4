/// Convert a portal `a{sv}` dict into owned values, preserving each value's type.
///
/// GTK serialises page setups with doubles for the paper dimensions and all four
/// margins. Flattening those to strings (or dropping them) made the dialog fail
/// to apply the caller's page setup, so keep the original variants intact.
fn to_owned_dict(dict: HashMap<String, Variant<'_>>) -> HashMap<String, OwnedVariant> {
    // Every input entry can yield at most one output entry, so the source
    // length is an exact capacity bound. Convert in place rather than through
    // a filter/collect that would grow the map repeatedly.
    let mut result = HashMap::with_capacity(dict.len());
    for (k, v) in dict {
        match v.try_into_owned() {
            Ok(owned) => {
                result.insert(k, owned);
            }
            Err(e) => {
                tracing::warn!(key = %k, error = %e, "Ignoring unsupported print option value");
            }
        }
    }
    result
}

use {
    super::gui::{ExecutePrintUi, PrintTokenProbe, PrintUi},
    crate::{
        core::{request::run_request, response::Response},
        gui::UiProxy,
    },
    std::{collections::HashMap, os::fd::AsFd},
    zbus::{
        DeserializeDict, Fd, ObjectServer, OwnedObjectPath, OwnedVariant, SerializeDict, Type,
        Variant, fdo, interface, message::Header,
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
#[zbus(signature = "dict")]
#[allow(dead_code)]
struct PreparePrintOptions {
    modal: Option<bool>,
    accept_label: Option<String>,
    pub activation_token: Option<String>,
}

#[derive(SerializeDict, Type, Debug, Default)]
#[zbus(signature = "dict")]
struct PreparePrintResults {
    settings: HashMap<String, OwnedVariant>,
    #[zbus(rename = "page-setup")]
    page_setup: HashMap<String, OwnedVariant>,
    token: u32,
    supported_output_file_formats: Option<Vec<String>>,
    has_current_page: Option<bool>,
    has_selected_pages: Option<bool>,
}

#[derive(DeserializeDict, Type, Debug, Default)]
#[zbus(signature = "dict")]
#[allow(dead_code)]
struct PrintOptions {
    modal: Option<bool>,
    token: Option<u32>,
    supported_output_file_formats: Option<Vec<String>>,
    pub activation_token: Option<String>,
}

#[derive(SerializeDict, Type, Debug, Default)]
#[zbus(signature = "dict")]
struct PrintResults {}

impl Print {
    async fn prepare_print_impl(
        &self,
        app_id: String,
        parent_window: String,
        title: String,
        settings: HashMap<String, Variant<'_>>,
        page_setup: HashMap<String, Variant<'_>>,
        options: PreparePrintOptions,
    ) -> Response<PreparePrintResults> {
        // Destructure once: nothing else reads `options`, so the token can be
        // moved straight into the UI task instead of cloned.
        let PreparePrintOptions {
            activation_token, ..
        } = options;
        let res = PrintUi {
            app_id,
            parent_window,
            activation_token,
            title,
            settings: to_owned_dict(settings),
            page_setup: to_owned_dict(page_setup),
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

        tracing::debug!(
            token,
            prepared = exists,
            showing_dialog = !exists,
            "Print token resolved"
        );

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

        // zbus borrows the received fd from the D-Bus message, which zbus
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
        settings: HashMap<String, Variant<'_>>,
        page_setup: HashMap<String, Variant<'_>>,
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
    use {super::*, zbus::Type};

    /// Pin the field order of the wire result. Breakdown on 64-bit:
    /// `settings` + `page_setup` (2 × 48) + `token` (4) +
    /// `supported_output_file_formats` (24) + two `Option<bool>` (1 each),
    /// rounded up to 128 by alignment.
    #[test]
    #[cfg(target_pointer_width = "64")]
    fn prepare_print_results_size_is_pinned() {
        assert_eq!(std::mem::size_of::<PreparePrintResults>(), 128);
    }

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
            zbus::{
                Variant,
                wire::{self, Endian, serialized::Context},
            },
        };

        let mut dict = HashMap::new();
        dict.insert("modal", Variant::from(true));
        dict.insert("accept_label", Variant::from("Print It"));

        let ctxt = Context::new(Endian::Little, 0);
        let encoded = wire::to_bytes(ctxt, &dict).unwrap();
        let options: PreparePrintOptions = encoded.deserialize().unwrap().0;

        assert_eq!(options.modal, Some(true));
        assert_eq!(options.accept_label.as_deref(), Some("Print It"));
    }

    #[test]
    fn test_print_options_deserialize() {
        use {
            std::collections::HashMap,
            zbus::{
                Variant,
                wire::{self, Endian, serialized::Context},
            },
        };

        let mut dict = HashMap::new();
        dict.insert("token", Variant::from(12345u32));

        let ctxt = Context::new(Endian::Little, 0);
        let encoded = wire::to_bytes(ctxt, &dict).unwrap();
        let options: PrintOptions = encoded.deserialize().unwrap().0;

        assert_eq!(options.token, Some(12345));
    }

    #[test]
    fn test_prepare_print_results_serialize() {
        use {
            std::collections::HashMap,
            zbus::{
                Variant,
                wire::{self, Endian, serialized::Context},
            },
        };

        let results = PreparePrintResults {
            token: 999,
            has_current_page: Some(true),
            page_setup: HashMap::from([("MarginTop".to_string(), OwnedVariant::from(11.0f64))]),
            ..Default::default()
        };

        let ctxt = Context::new(Endian::Little, 0);
        let encoded = wire::to_bytes(ctxt, &results).unwrap();
        let decoded: HashMap<String, Variant> = encoded.deserialize().unwrap().0;

        assert_eq!(
            decoded.get("token").unwrap().try_clone().unwrap(),
            Variant::from(999u32)
        );
        assert_eq!(
            decoded
                .get("has_current_page")
                .unwrap()
                .try_clone()
                .unwrap(),
            Variant::from(true)
        );
        let page_setup = decoded.get("page-setup").unwrap().try_clone().unwrap();
        let Variant::Dict(dict) = page_setup else {
            panic!("page-setup must be a dict");
        };
        let key = String::from("MarginTop");
        let margin: Variant = dict.get(&key).unwrap().unwrap();
        assert_eq!(margin, Variant::from(11.0f64));
    }

    /// This is the exact site of the reported bug: the old `flatten_dict`
    /// kept only strings, so GTK's double-valued margins never reached the
    /// dialog. Guard the conversion itself, not just its downstream users.
    #[test]
    fn to_owned_dict_preserves_non_string_values() {
        let dict = HashMap::from([
            ("Name".to_string(), Variant::from("iso_a4_210x297mm")),
            ("MarginTop".to_string(), Variant::from(11.0f64)),
            ("Width".to_string(), Variant::from(210.0f64)),
            ("Collate".to_string(), Variant::from(true)),
        ]);

        let owned = to_owned_dict(dict);
        assert_eq!(owned.len(), 4, "no value type may be dropped");
        assert_eq!(
            f64::try_from(&owned["MarginTop"]).unwrap(),
            11.0,
            "margins must survive as doubles"
        );
        assert_eq!(f64::try_from(&owned["Width"]).unwrap(), 210.0);
        assert!(bool::try_from(&owned["Collate"]).unwrap());
        assert_eq!(
            String::try_from(owned["Name"].try_clone().unwrap()).unwrap(),
            "iso_a4_210x297mm"
        );
    }
}
