use {
    crate::core::{request::run_request, response::Response},
    gtk4::{gio::AppInfo, glib, prelude::AppLaunchContextExt},
    zbus::{
        ObjectServer, interface,
        zvariant::{DeserializeDict, OwnedObjectPath, SerializeDict, Type},
    },
};

pub struct Email;

impl Email {
    pub fn new() -> Self {
        Self
    }
}

#[derive(DeserializeDict, Type, Debug, Default)]
#[zvariant(signature = "dict")]
struct ComposeEmailOptions {
    address: Option<String>,
    addresses: Option<Vec<String>>,
    cc: Option<Vec<String>>,
    bcc: Option<Vec<String>>,
    subject: Option<String>,
    body: Option<String>,
    attachments: Option<Vec<String>>,
    activation_token: Option<String>,
}

#[derive(SerializeDict, Type, Debug, Default)]
#[zvariant(signature = "dict")]
struct EmailResults {}

impl Email {
    async fn compose_email_impl(
        &self,
        _app_id: String,
        _parent_window: String,
        options: ComposeEmailOptions,
    ) -> Response<EmailResults> {
        let url = build_mailto_url(&options);

        let launch_context = gtk4::gio::AppLaunchContext::new();
        if let Some(token) = &options.activation_token {
            launch_context.setenv("DESKTOP_STARTUP_ID", token);
            launch_context.setenv("XDG_ACTIVATION_TOKEN", token);
        }

        match AppInfo::launch_default_for_uri(&url, Some(&launch_context)) {
            Ok(_) => Response::success(EmailResults::default()),
            Err(e) => {
                log::error!("ComposeEmail failed: {}", anyhow::Error::new(e));
                Response::cancelled()
            }
        }
    }
}

fn build_mailto_url(options: &ComposeEmailOptions) -> String {
    let mut url = String::from("mailto:");
    let mut all_addresses = Vec::new();
    if let Some(address) = &options.address {
        all_addresses.push(address.clone());
    }
    if let Some(addresses) = &options.addresses {
        all_addresses.extend(addresses.iter().cloned());
    }
    if !all_addresses.is_empty() {
        url.push_str(&all_addresses.join(","));
    }

    url.push('?');

    if let Some(cc) = &options.cc {
        for addr in cc {
            url.push_str(&format!(
                "cc={}&",
                glib::uri_escape_string(addr, None::<&str>, true)
            ));
        }
    }
    if let Some(bcc) = &options.bcc {
        for addr in bcc {
            url.push_str(&format!(
                "bcc={}&",
                glib::uri_escape_string(addr, None::<&str>, true)
            ));
        }
    }
    if let Some(subject) = &options.subject {
        url.push_str(&format!(
            "subject={}&",
            glib::uri_escape_string(subject, None::<&str>, true)
        ));
    }
    if let Some(body) = &options.body {
        url.push_str(&format!(
            "body={}&",
            glib::uri_escape_string(body, None::<&str>, true)
        ));
    }
    if let Some(attachments) = &options.attachments {
        for att in attachments {
            url.push_str(&format!(
                "attachment={}&",
                glib::uri_escape_string(att, None::<&str>, true)
            ));
        }
    }

    // Remove trailing '?' or '&'
    url.pop();

    url
}

#[interface(name = "org.freedesktop.impl.portal.Email")]
impl Email {
    async fn compose_email(
        &self,
        handle: OwnedObjectPath,
        app_id: String,
        parent_window: String,
        options: ComposeEmailOptions,
        #[zbus(object_server)] server: &ObjectServer,
    ) -> Response<EmailResults> {
        run_request(
            server,
            handle,
            self.compose_email_impl(app_id, parent_window, options),
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zbus::zvariant::Type;

    #[test]
    fn test_compose_url_basic() {
        let options = ComposeEmailOptions {
            addresses: Some(vec!["user@example.com".to_string()]),
            subject: Some("Hello".to_string()),
            body: Some("World".to_string()),
            ..Default::default()
        };
        assert_eq!(
            build_mailto_url(&options),
            "mailto:user@example.com?subject=Hello&body=World"
        );
    }

    #[test]
    fn test_compose_url_multiple_addresses() {
        let options = ComposeEmailOptions {
            address: Some("single@example.com".to_string()),
            addresses: Some(vec![
                "foo@example.com".to_string(),
                "bar@example.com".to_string(),
            ]),
            ..Default::default()
        };
        assert_eq!(
            build_mailto_url(&options),
            "mailto:single@example.com,foo@example.com,bar@example.com"
        );
    }

    #[test]
    fn test_compose_url_cc_bcc() {
        let options = ComposeEmailOptions {
            cc: Some(vec!["cc1@example.com".to_string()]),
            bcc: Some(vec!["bcc1@example.com".to_string()]),
            ..Default::default()
        };
        assert_eq!(
            build_mailto_url(&options),
            "mailto:?cc=cc1%40example.com&bcc=bcc1%40example.com"
        );
    }

    #[test]
    fn test_compose_url_special_chars() {
        let options = ComposeEmailOptions {
            subject: Some("Hello & Welcome=".to_string()),
            body: Some("Space here".to_string()),
            ..Default::default()
        };
        assert_eq!(
            build_mailto_url(&options),
            "mailto:?subject=Hello%20%26%20Welcome%3D&body=Space%20here"
        );
    }

    #[test]
    fn test_compose_url_empty() {
        let options = ComposeEmailOptions::default();
        assert_eq!(build_mailto_url(&options), "mailto:");
    }

    #[test]
    fn test_compose_email_options_signature() {
        assert_eq!(ComposeEmailOptions::SIGNATURE, "a{sv}");
    }

    #[test]
    fn test_email_results_signature() {
        assert_eq!(EmailResults::SIGNATURE, "a{sv}");
    }
}
