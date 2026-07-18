use {
    super::gui::AppChooserUi,
    crate::{
        core::{request::run_request, response::Response},
        gui::{UiError, UiProxy},
    },
    async_channel::Sender,
    std::{collections::HashMap, sync::Mutex},
    zbus::{
        interface,
        zvariant::{DeserializeDict, OwnedObjectPath, SerializeDict, Type},
    },
};

#[derive(DeserializeDict, Type, Debug)]
#[zvariant(signature = "dict")]
pub struct ChooseApplicationOptions {
    last_choice: Option<String>,
    modal: Option<bool>,
    content_type: Option<String>,
    uri: Option<String>,
    filename: Option<String>,
    activation_token: Option<String>,
}

#[derive(SerializeDict, Type, Debug, Default)]
#[zvariant(signature = "dict")]
pub struct ChooseApplicationResults {
    choice: Option<String>,
    activation_token: Option<String>,
}

pub struct AppChooser {
    proxy: UiProxy,
    // The AppChooser portal allows the frontend to update the list of choices
    // while the dialog is open (e.g., if it finds new apps). We maintain a map
    // of active request handles to channel senders so we can pipe these updates
    // to the running GTK dialogs.
    active_dialogs: std::sync::Arc<Mutex<HashMap<OwnedObjectPath, Sender<Vec<String>>>>>,
}

impl AppChooser {
    pub fn new(proxy: &UiProxy) -> Self {
        Self {
            proxy: proxy.clone(),
            active_dialogs: std::sync::Arc::new(Mutex::new(HashMap::new())),
        }
    }

    async fn choose_application_impl(
        &self,
        handle: OwnedObjectPath,
        app_id: String,
        parent_window: String,
        choices: Vec<String>,
        options: ChooseApplicationOptions,
    ) -> Response<ChooseApplicationResults> {
        struct ActiveDialogGuard {
            active_dialogs: std::sync::Arc<Mutex<HashMap<OwnedObjectPath, Sender<Vec<String>>>>>,
            handle: OwnedObjectPath,
        }

        impl Drop for ActiveDialogGuard {
            fn drop(&mut self) {
                if let Ok(mut lock) = self.active_dialogs.lock() {
                    lock.remove(&self.handle);
                }
            }
        }

        let (update_sender, update_receiver) = async_channel::bounded(10);

        if let Ok(mut lock) = self.active_dialogs.lock() {
            lock.insert(handle.clone(), update_sender);
        }

        let _guard = ActiveDialogGuard {
            active_dialogs: self.active_dialogs.clone(),
            handle: handle.clone(),
        };

        let ui = AppChooserUi {
            app_id,
            parent_window,
            activation_token: options.activation_token.clone(),
            title: rust_i18n::t!("choose_an_application").to_string(),
            choices,
            filename: options.filename,
            content_type: options.content_type,
        };

        let res = ui.run(&self.proxy, update_receiver).await;

        match res {
            Ok(result) => {
                let res = ChooseApplicationResults {
                    choice: Some(result.choice),
                    activation_token: result.activation_token.or(options.activation_token),
                };
                Response::success(res)
            }
            Err(UiError::Closed) | Err(UiError::Rejected) => Response::cancelled(),
        }
    }
}

/// The D-Bus interface implementation for `org.freedesktop.impl.portal.AppChooser`.
///
/// This portal provides a UI for the user to select an application to open a file
/// or handle a specific content type.
#[interface(name = "org.freedesktop.impl.portal.AppChooser")]
impl AppChooser {
    #[zbus(name = "ChooseApplication")]
    async fn choose_application(
        &self,
        handle: OwnedObjectPath,
        app_id: String,
        parent_window: String,
        choices: Vec<String>,
        options: ChooseApplicationOptions,
        #[zbus(object_server)] server: &zbus::ObjectServer,
    ) -> Response<ChooseApplicationResults> {
        run_request(
            server,
            handle.clone(),
            self.choose_application_impl(handle, app_id, parent_window, choices, options),
        )
        .await
    }

    #[zbus(name = "UpdateChoices")]
    async fn update_choices(
        &self,
        handle: OwnedObjectPath,
        choices: Vec<String>,
    ) -> zbus::fdo::Result<()> {
        log::info!("UpdateChoices called for handle: {}", handle.as_str());
        // Look up the channel sender for this specific request handle.
        // If found, send the new list of choices to the GTK task.
        if let Ok(lock) = self.active_dialogs.lock() {
            if let Some(sender) = lock.get(&handle) {
                let _ = sender.try_send(choices);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use {super::*, zbus::zvariant::Type};

    #[test]
    fn test_choose_application_options_signature() {
        assert_eq!(ChooseApplicationOptions::SIGNATURE, "a{sv}");
    }

    #[test]
    fn test_choose_application_results_signature() {
        assert_eq!(ChooseApplicationResults::SIGNATURE, "a{sv}");
    }

    #[tokio::test]
    async fn test_update_choices_sends_message() {
        let proxy = UiProxy {
            context: gtk4::glib::MainContext::default(),
        };
        let chooser = AppChooser::new(&proxy);
        let (sender, receiver) = async_channel::bounded(1);

        let path = OwnedObjectPath::try_from("/test/handle").unwrap();

        {
            let mut lock = chooser.active_dialogs.lock().unwrap();
            lock.insert(path.clone(), sender);
        }
        let choices = vec!["choice1".to_string(), "choice2".to_string()];

        let res = chooser.update_choices(path, choices.clone()).await;
        assert!(res.is_ok());

        let received = receiver.try_recv().unwrap();
        assert_eq!(received, choices);
    }

    #[tokio::test]
    async fn test_update_choices_unknown_handle() {
        let proxy = UiProxy {
            context: gtk4::glib::MainContext::default(),
        };
        let chooser = AppChooser::new(&proxy);
        let path = OwnedObjectPath::try_from("/unknown/handle").unwrap();

        // Should succeed but do nothing
        let res = chooser.update_choices(path, vec![]).await;
        assert!(res.is_ok());
    }
}
