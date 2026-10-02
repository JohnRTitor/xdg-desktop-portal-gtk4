use {
    crate::core::{request::run_request, response::Response},
    gtk4::{gio, gio::AppInfo, glib, prelude::AppLaunchContextExt},
    zbus::{
        ObjectServer, fdo, interface,
        message::Header,
        zvariant::{DeserializeDict, OwnedObjectPath, SerializeDict, Type},
    },
};

/// D-Bus interface wrapper for the Email portal.
///
/// This struct holds no state because it simply constructs a `mailto:` URI
/// and uses GIO to launch the host system's default email client.
pub struct Email {
    session_manager: crate::core::session_manager::SessionManager,
}

impl Email {
    pub fn new(session_manager: crate::core::session_manager::SessionManager) -> Self {
        Self { session_manager }
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
        // The Email portal doesn't show its own UI; instead, it delegates to the host's
        // default mail client using a `mailto:` URI.
        let url = build_mailto_url(&options);

        let launch_context = gio::AppLaunchContext::new();
        if let Some(token) = &options.activation_token {
            // Pass the Wayland/X11 activation token so the mail client can raise its window.
            launch_context.setenv("DESKTOP_STARTUP_ID", token);
            launch_context.setenv("XDG_ACTIVATION_TOKEN", token);
        }

        // We rely on GIO to determine the default application for `mailto:` URIs.
        match AppInfo::launch_default_for_uri(&url, Some(&launch_context)) {
            Ok(_) => Response::success(EmailResults::default()),
            Err(e) => {
                tracing::error!(error = %e, "ComposeEmail failed");
                // Launching the mail client is not a dialog, so there is no user refusal
                // to report. The contract reserves 1 for "the user cancelled the
                // interaction" (`org.freedesktop.portal.Request.xml`, the
                // `Response` signal); nobody was shown anything here, so a failed
                // launch is the "some other way" case.
                Response::other()
            }
        }
    }
}

/// Appends one `key=` parameter holding a comma-separated recipient list.
///
/// RFC 6068 gives `cc` and `bcc` exactly one parameter whose value is
/// `addr-spec *("," addr-spec)`. Emitting `cc=a&cc=b` instead is a repeated
/// query key, which is not what the grammar describes, and mail clients differ
/// on what they do with it — commonly honouring one occurrence and silently
/// dropping the other. So a caller CC'ing two people reached the user's mail
/// client with one of them missing.
///
/// This is why every pre-existing test here passed a single address: with one
/// recipient a repeated key and a comma-joined value are the same string, so
/// the shape of the bug is invisible until a second address is present.
///
/// The addresses are still percent-escaped, which the reference does not do.
/// That is deliberate: a raw `&` in an address would otherwise terminate the
/// parameter and let the address inject query fields of its own.
fn append_recipients(url: &mut String, key: &str, addrs: &[String]) {
    use std::fmt::Write;

    if addrs.is_empty() {
        return;
    }
    let _ = write!(url, "{key}=");
    for (i, addr) in addrs.iter().enumerate() {
        if i > 0 {
            url.push(',');
        }
        let _ = write!(url, "{}", glib::uri_escape_string(addr, None::<&str>, true));
    }
    url.push('&');
}

fn build_mailto_url(options: &ComposeEmailOptions) -> String {
    let mut url = String::from("mailto:");
    let all_addresses: Vec<&str> = options
        .address
        .as_deref()
        .into_iter()
        .chain(options.addresses.iter().flatten().map(|s| s.as_str()))
        .collect();

    if !all_addresses.is_empty() {
        url.push_str(&all_addresses.join(","));
    }

    url.push('?');

    use std::fmt::Write;

    if let Some(cc) = &options.cc {
        append_recipients(&mut url, "cc", cc);
    }
    if let Some(bcc) = &options.bcc {
        append_recipients(&mut url, "bcc", bcc);
    }
    if let Some(subject) = &options.subject {
        let _ = write!(
            &mut url,
            "subject={}&",
            glib::uri_escape_string(subject, None::<&str>, true)
        );
    }
    if let Some(body) = &options.body {
        let _ = write!(
            &mut url,
            "body={}&",
            glib::uri_escape_string(body, None::<&str>, true)
        );
    }
    if let Some(attachments) = &options.attachments {
        for att in attachments {
            let _ = write!(
                &mut url,
                "attachment={}&",
                glib::uri_escape_string(att, None::<&str>, true)
            );
        }
    }

    // Remove trailing '?' or '&'
    url.pop();

    url
}

/// The D-Bus interface implementation for `org.freedesktop.impl.portal.Email`.
///
/// Provides a way for sandboxed applications to compose emails.
#[interface(name = "org.freedesktop.impl.portal.Email")]
impl Email {
    #[tracing::instrument(skip_all, fields(app_id = %app_id, handle = %handle.as_str()))]
    async fn compose_email(
        &self,
        #[zbus(header)] header: Header<'_>,
        handle: OwnedObjectPath,
        app_id: String,
        parent_window: String,
        options: ComposeEmailOptions,
        #[zbus(object_server)] server: &ObjectServer,
    ) -> Result<Response<EmailResults>, fdo::Error> {
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
            self.compose_email_impl(app_id.clone(), parent_window, options),
        )
        .await)
    }
}

#[cfg(test)]
mod tests {
    use {super::*, zbus::zvariant::Type};

    #[test]
    fn test_compose_url_basic() {
        let options = ComposeEmailOptions {
            addresses: Some(vec!["user@example.com".into()]),
            subject: Some("Hello".into()),
            body: Some("World".into()),
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
            address: Some("single@example.com".into()),
            addresses: Some(vec!["foo@example.com".into(), "bar@example.com".into()]),
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
            cc: Some(vec!["cc1@example.com".into()]),
            bcc: Some(vec!["bcc1@example.com".into()]),
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
            subject: Some("Hello & Welcome=".into()),
            body: Some("Space here".into()),
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
    fn test_compose_url_attachments() {
        let options = ComposeEmailOptions {
            attachments: Some(vec![
                "file:///tmp/doc.txt".into(),
                "file:///tmp/image.png".into(),
            ]),
            ..Default::default()
        };
        assert_eq!(
            build_mailto_url(&options),
            "mailto:?attachment=file%3A%2F%2F%2Ftmp%2Fdoc.txt&attachment=file%3A%2F%2F%2Ftmp%2Fimage.png"
        );
    }

    #[test]
    fn test_compose_email_results_signature() {
        assert_eq!(EmailResults::SIGNATURE, "a{sv}");
    }

    /// The regression: `cc` and `bcc` are one parameter each, not one per
    /// recipient. Every test above passes a single address, where a repeated
    /// key and a comma-joined value are indistinguishable -- which is exactly
    /// why this went unnoticed.
    #[test]
    fn test_compose_url_multiple_cc_recipients_share_one_key() {
        let options = ComposeEmailOptions {
            cc: Some(vec!["one@example.com".into(), "two@example.com".into()]),
            ..Default::default()
        };
        let url = build_mailto_url(&options);
        assert_eq!(
            url, "mailto:?cc=one%40example.com,two%40example.com",
            "cc must be a single comma-joined parameter"
        );
        assert_eq!(
            url.matches("cc=").count(),
            1,
            "a repeated cc= key makes clients drop recipients: {url}"
        );
    }

    #[test]
    fn test_compose_url_multiple_bcc_recipients_share_one_key() {
        let options = ComposeEmailOptions {
            bcc: Some(vec!["one@example.com".into(), "two@example.com".into()]),
            ..Default::default()
        };
        let url = build_mailto_url(&options);
        assert_eq!(url, "mailto:?bcc=one%40example.com,two%40example.com");
        assert_eq!(url.matches("bcc=").count(), 1, "{url}");
    }

    /// Both fields together must not bleed into one another, and the ordering
    /// has to stay cc, bcc, subject, body.
    #[test]
    fn test_compose_url_cc_and_bcc_stay_separate() {
        let options = ComposeEmailOptions {
            cc: Some(vec!["a@example.com".into(), "b@example.com".into()]),
            bcc: Some(vec!["c@example.com".into(), "d@example.com".into()]),
            subject: Some("Hi".into()),
            ..Default::default()
        };
        assert_eq!(
            build_mailto_url(&options),
            "mailto:?cc=a%40example.com,b%40example.com&bcc=c%40example.com,d%40example.com&subject=Hi"
        );
    }

    /// An empty list must not emit a bare `cc=` with no value, which some
    /// clients read as an empty recipient.
    #[test]
    fn test_compose_url_empty_recipient_lists_are_omitted() {
        let options = ComposeEmailOptions {
            cc: Some(vec![]),
            bcc: Some(vec![]),
            subject: Some("Hi".into()),
            ..Default::default()
        };
        assert_eq!(build_mailto_url(&options), "mailto:?subject=Hi");
    }

    /// A comma inside a single address must stay escaped rather than being
    /// read as a separator, so one recipient cannot split into two.
    #[test]
    fn test_compose_url_comma_in_address_is_escaped() {
        let options = ComposeEmailOptions {
            cc: Some(vec!["weird,name@example.com".into()]),
            ..Default::default()
        };
        let url = build_mailto_url(&options);
        assert_eq!(url, "mailto:?cc=weird%2Cname%40example.com");
        assert_eq!(
            url.matches("cc=").count(),
            1,
            "an unescaped comma would let one address become two: {url}"
        );
    }

    /// `attachment` is a repeatable field and stays one key per file, matching
    /// the reference implementation.
    #[test]
    fn test_compose_url_attachments_repeat_the_key() {
        let options = ComposeEmailOptions {
            attachments: Some(vec!["file:///a.txt".into(), "file:///b.txt".into()]),
            ..Default::default()
        };
        assert_eq!(
            build_mailto_url(&options),
            "mailto:?attachment=file%3A%2F%2F%2Fa.txt&attachment=file%3A%2F%2F%2Fb.txt"
        );
    }

    #[test]
    fn test_compose_email_options_deserialize() {
        use {
            std::collections::HashMap,
            zbus::zvariant::{self, Endian, Value, serialized::Context},
        };

        let mut dict = HashMap::new();
        dict.insert("address", Value::from("test@example.com"));
        dict.insert("subject", Value::from("Test Subject"));

        let ctxt = Context::new_dbus(Endian::Little, 0);
        let encoded = zvariant::to_bytes(ctxt, &dict).unwrap();
        let options: ComposeEmailOptions = encoded.deserialize().unwrap().0;

        assert_eq!(options.address.as_deref(), Some("test@example.com"));
        assert_eq!(options.subject.as_deref(), Some("Test Subject"));
    }

    #[test]
    fn test_email_results_serialize() {
        use {
            std::collections::HashMap,
            zbus::zvariant::{self, Endian, Value, serialized::Context},
        };

        let results = EmailResults::default();
        let ctxt = Context::new_dbus(Endian::Little, 0);
        let encoded = zvariant::to_bytes(ctxt, &results).unwrap();
        let decoded: HashMap<String, Value> = encoded.deserialize().unwrap().0;

        assert!(decoded.is_empty());
    }
}
