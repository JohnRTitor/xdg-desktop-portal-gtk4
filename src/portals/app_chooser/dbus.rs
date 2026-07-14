use zbus::zvariant::{DeserializeDict, SerializeDict, Type, OwnedObjectPath};
use zbus::interface;
use std::collections::HashMap;
use std::sync::Mutex;
use async_channel::Sender;
use crate::{
    gui::UiProxy,
    core::{request::run_request, response::Response},
};
use super::gui::{AppChooserUi, AppChooserError};

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
    active_dialogs: Mutex<HashMap<String, Sender<Vec<String>>>>,
}

impl AppChooser {
    pub fn new(proxy: &UiProxy) -> Self {
        Self { 
            proxy: proxy.clone(),
            active_dialogs: Mutex::new(HashMap::new()),
        }
    }

    async fn choose_application_impl(
        &self,
        handle_str: String,
        app_id: String,
        parent_window: String,
        choices: Vec<String>,
        options: ChooseApplicationOptions,
    ) -> Response<ChooseApplicationResults> {
        let (update_sender, update_receiver) = async_channel::bounded(10);
        
        if let Ok(mut lock) = self.active_dialogs.lock() {
            lock.insert(handle_str.clone(), update_sender);
        }

        let ui = AppChooserUi {
            app_id,
            parent_window,
            title: rust_i18n::t!("Choose an application").to_string(),
            choices,
            filename: options.filename,
            content_type: options.content_type,
        };
        
        let res = ui.run(&self.proxy, update_receiver).await;

        if let Ok(mut lock) = self.active_dialogs.lock() {
            lock.remove(&handle_str);
        }

        match res {
            Ok(result) => {
                let res = ChooseApplicationResults {
                    choice: Some(result.choice),
                    activation_token: options.activation_token,
                };
                Response::success(res)
            }
            Err(AppChooserError::Closed) | Err(AppChooserError::Rejected) => {
                Response::cancelled()
            }
        }
    }
}

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
        let handle_str = handle.as_str().to_string();
        run_request(
            server,
            handle,
            self.choose_application_impl(handle_str, app_id, parent_window, choices, options)
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
        if let Ok(lock) = self.active_dialogs.lock() {
            if let Some(sender) = lock.get(handle.as_str()) {
                let _ = sender.try_send(choices);
            }
        }
        Ok(())
    }
}
