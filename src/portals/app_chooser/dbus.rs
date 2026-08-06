use {
    super::gui::AppChooserUi,
    crate::{
        core::{request::run_request, response::Response},
        gui::{UiError, UiProxy},
    },
    parking_lot::Mutex,
    std::collections::HashMap,
    tokio::sync::mpsc::Sender,
    zbus::{
        ObjectServer, interface,
        zvariant::{DeserializeDict, OwnedObjectPath, SerializeDict, Type},
    },
};

#[derive(DeserializeDict, Type, Debug)]
#[zvariant(signature = "dict")]
#[allow(dead_code)]
pub struct ChooseApplicationOptions {
    #[allow(dead_code)]
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

const CHANNEL_BUFFER_SIZE: usize = 10;

/// D-Bus interface wrapper for the AppChooser portal.
///
/// This struct manages active app chooser dialogs. It maintains a mapping
/// between the D-Bus object path of the request and a channel sender that
/// pipes dynamically discovered application choices to the GTK frontend.
pub struct AppChooser {
    proxy: UiProxy,
    /// The AppChooser portal allows the frontend to update the list of choices
    /// while the dialog is open (e.g., if it finds new apps). We maintain a map
    /// of active request handles to channel senders so we can pipe these updates
    /// to the running GTK dialogs.
    ///
    /// # Threading & Locking
    ///
    /// A `parking_lot::Mutex` is sufficient here (rather than RwLock or Tokio Mutex)
    /// because insertion and removal only happen during setup and teardown, and
    /// we do not hold the lock across `.await` points.
    active_dialogs: std::sync::Arc<Mutex<HashMap<OwnedObjectPath, Sender<Vec<String>>>>>,
    session_manager: crate::core::session_manager::SessionManager,
}

impl AppChooser {
    pub fn new(
        proxy: &UiProxy,
        session_manager: crate::core::session_manager::SessionManager,
    ) -> Self {
        Self {
            proxy: proxy.clone(),
            active_dialogs: std::sync::Arc::new(Mutex::new(HashMap::new())),
            session_manager,
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
        // Guard pattern: Ensures that the dialog is removed from the `active_dialogs` map
        // when this method exits, regardless of whether it returned successfully, was cancelled,
        // or panicked. This prevents a memory leak of stale handles.
        struct ActiveDialogGuard {
            active_dialogs: std::sync::Arc<Mutex<HashMap<OwnedObjectPath, Sender<Vec<String>>>>>,
            handle: OwnedObjectPath,
        }

        impl Drop for ActiveDialogGuard {
            fn drop(&mut self) {
                let mut lock = self.active_dialogs.lock();
                lock.remove(&self.handle);
            }
        }

        let (update_sender, update_receiver) = tokio::sync::mpsc::channel(CHANNEL_BUFFER_SIZE);

        {
            let mut lock = self.active_dialogs.lock();
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
    #[tracing::instrument(skip_all, fields(app_id = %app_id, handle = %handle.as_str()))]
    async fn choose_application(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        handle: OwnedObjectPath,
        app_id: String,
        parent_window: String,
        choices: Vec<String>,
        options: ChooseApplicationOptions,
        #[zbus(object_server)] server: &ObjectServer,
    ) -> Result<Response<ChooseApplicationResults>, zbus::fdo::Error> {
        let sender = header
            .sender()
            .ok_or_else(|| zbus::fdo::Error::Failed("Missing sender".to_string()))?
            .to_string();
        Ok(run_request(
            server,
            self.session_manager.clone(),
            &app_id,
            &sender,
            handle.clone(),
            self.choose_application_impl(handle, app_id.clone(), parent_window, choices, options),
        )
        .await)
    }

    #[zbus(name = "UpdateChoices")]
    async fn update_choices(
        &self,
        handle: OwnedObjectPath,
        choices: Vec<String>,
    ) -> zbus::fdo::Result<()> {
        tracing::info!("UpdateChoices called for handle: {}", handle.as_str());
        // Look up the channel sender for this specific request handle.
        // If found, send the new list of choices to the GTK task.
        // This runs on the Tokio thread, while the receiving end runs on the GTK thread.
        if let Some(sender) = self.active_dialogs.lock().get(&handle) {
            let _ = sender.try_send(choices);
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
            sender: tokio::sync::mpsc::unbounded_channel().0,
        };
        let conn = match zbus::Connection::session().await {
            Ok(c) => c,
            Err(_) => return,
        };
        let chooser = AppChooser::new(
            &proxy,
            crate::core::session_manager::SessionManager::new(conn, 10),
        );
        let (sender, mut receiver) = tokio::sync::mpsc::channel(1);

        let path = OwnedObjectPath::try_from("/test/handle").unwrap();

        {
            let mut lock = chooser.active_dialogs.lock();
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
            sender: tokio::sync::mpsc::unbounded_channel().0,
        };
        let conn = match zbus::Connection::session().await {
            Ok(c) => c,
            Err(_) => return,
        };
        let chooser = AppChooser::new(
            &proxy,
            crate::core::session_manager::SessionManager::new(conn, 10),
        );
        let path = OwnedObjectPath::try_from("/unknown/handle").unwrap();

        // Should succeed but do nothing
        let res = chooser.update_choices(path, vec![]).await;
        assert!(res.is_ok());
    }

    #[test]
    fn test_choose_application_options_deserialize() {
        use {
            std::collections::HashMap,
            zbus::zvariant::{Endian, Value, serialized::Context},
        };

        let mut dict = HashMap::new();
        dict.insert("modal", Value::from(true));
        dict.insert("uri", Value::from("file:///tmp/test.txt"));

        let ctxt = Context::new_dbus(Endian::Little, 0);
        let encoded = zbus::zvariant::to_bytes(ctxt, &dict).unwrap();
        let options: ChooseApplicationOptions = encoded.deserialize().unwrap().0;

        assert_eq!(options.modal, Some(true));
        assert_eq!(options.uri.as_deref(), Some("file:///tmp/test.txt"));
    }

    #[test]
    fn test_choose_application_results_serialize() {
        use {
            std::collections::HashMap,
            zbus::zvariant::{Endian, Value, serialized::Context},
        };

        let results = ChooseApplicationResults {
            choice: Some("app.desktop".to_string()),
            activation_token: Some("token123".to_string()),
        };

        let ctxt = Context::new_dbus(Endian::Little, 0);
        let encoded = zbus::zvariant::to_bytes(ctxt, &results).unwrap();
        let decoded: HashMap<String, Value> = encoded.deserialize().unwrap().0;

        assert_eq!(
            decoded.get("choice").unwrap().try_clone().unwrap(),
            Value::from("app.desktop")
        );
        assert_eq!(
            decoded
                .get("activation_token")
                .unwrap()
                .try_clone()
                .unwrap(),
            Value::from("token123")
        );
    }
}
