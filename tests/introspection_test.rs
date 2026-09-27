//! D-Bus introspection conformance test.
//!
//! Every other integration test in this repository exercises *behaviour*, but
//! almost none of them exercise the **wire contract**: which interfaces are
//! actually exported at `/org/freedesktop/portal/desktop`, and whether their
//! method signatures and property names match
//! `org.freedesktop.impl.portal.*.xml` from xdg-desktop-portal.
//!
//! This test closes that gap. It registers every portal object on a private
//! session bus, introspects the object, and compares the result against the
//! upstream contract transcribed in [`EXPECTED`] below.
//!
//! It exists because D-Bus property and method names are **case-sensitive** and
//! because `#[zbus(property)]` silently derives the wire name from the Rust
//! function name, capitalising it. That is exactly how
//! `Clipboard.Version` and `Settings.Version` came to be exported where the
//! contract says `version`.

use {std::collections::BTreeSet, xdg_desktop_portal_gtk4::gui::UiProxy};

const IMPL_PREFIX: &str = "org.freedesktop.impl.portal.";

/// Transcribed from xdg-desktop-portal `data/org.freedesktop.impl.portal.*.xml`.
///
/// Key: interface name. Value: `(methods, properties)` where `methods` maps a
/// method name to its ordered argument type list (in-args then out-args, with
/// the out tuple flattened as it appears on the wire) and `properties` is the
/// set of declared property names.
///
/// The out-argument is always the single `(ua{sv})` struct for request-returning
/// methods, `u` for `Inhibit.CreateMonitor` / `DynamicLauncher.RequestInstallToken`,
/// `h` for the clipboard fd methods, `v` / `a{sa{sv}}` for Settings, and absent
/// where the contract declares no return.
#[allow(clippy::type_complexity)]
const EXPECTED: &[(&str, &[(&str, &[&str])], &[&str])] = &[
    (
        "Access",
        &[(
            "AccessDialog",
            &["o", "s", "s", "s", "s", "s", "a{sv}", "(ua{sv})"],
        )],
        &[],
    ),
    (
        "Account",
        &[("GetUserInformation", &["o", "s", "s", "a{sv}", "(ua{sv})"])],
        &[],
    ),
    (
        "AppChooser",
        &[
            (
                "ChooseApplication",
                &["o", "s", "s", "as", "a{sv}", "(ua{sv})"],
            ),
            ("UpdateChoices", &["o", "as"]),
        ],
        &[],
    ),
    (
        "Clipboard",
        &[
            ("RequestClipboard", &["o", "a{sv}"]),
            ("SetSelection", &["o", "a{sv}"]),
            ("SelectionWrite", &["o", "u", "h"]),
            ("SelectionWriteDone", &["o", "u", "b"]),
            ("SelectionRead", &["o", "s", "h"]),
        ],
        &["version"],
    ),
    (
        "DynamicLauncher",
        &[
            (
                "PrepareInstall",
                &["o", "s", "s", "s", "v", "a{sv}", "(ua{sv})"],
            ),
            ("RequestInstallToken", &["s", "a{sv}", "u"]),
        ],
        &["SupportedLauncherTypes", "version"],
    ),
    (
        "Email",
        &[("ComposeEmail", &["o", "s", "s", "a{sv}", "(ua{sv})"])],
        &[],
    ),
    (
        "FileChooser",
        &[
            ("OpenFile", &["o", "s", "s", "s", "a{sv}", "(ua{sv})"]),
            ("SaveFile", &["o", "s", "s", "s", "a{sv}", "(ua{sv})"]),
            ("SaveFiles", &["o", "s", "s", "s", "a{sv}", "(ua{sv})"]),
        ],
        &[],
    ),
    (
        "Inhibit",
        &[
            ("Inhibit", &["o", "s", "s", "u", "a{sv}"]),
            ("CreateMonitor", &["o", "o", "s", "s", "u"]),
            ("QueryEndResponse", &["o"]),
        ],
        &[],
    ),
    (
        "Lockdown",
        &[],
        &[
            "disable-application-handlers",
            "disable-camera",
            "disable-location",
            "disable-microphone",
            "disable-printing",
            "disable-save-to-disk",
            "disable-sound-output",
        ],
    ),
    (
        "Notification",
        &[
            ("AddNotification", &["s", "s", "a{sv}"]),
            ("RemoveNotification", &["s", "s"]),
        ],
        &["SupportedOptions", "version"],
    ),
    (
        "Print",
        &[
            (
                "PreparePrint",
                &["o", "s", "s", "s", "a{sv}", "a{sv}", "a{sv}", "(ua{sv})"],
            ),
            ("Print", &["o", "s", "s", "s", "h", "a{sv}", "(ua{sv})"]),
        ],
        &[],
    ),
    (
        "Settings",
        &[
            ("Read", &["s", "s", "v"]),
            ("ReadAll", &["as", "a{sa{sv}}"]),
        ],
        &["version"],
    ),
    (
        "Usb",
        &[(
            "AcquireDevices",
            &["o", "s", "s", "a(sa{sv}a{sv})", "a{sv}", "(ua{sv})"],
        )],
        &["version"],
    ),
];

/// A deliberately minimal scanner for the subset of introspection XML zbus
/// emits. Avoids pulling in an XML parser for one test.
struct Scanned {
    methods: Vec<(String, Vec<String>)>,
    properties: BTreeSet<String>,
}

fn scan_interface(xml: &str, iface: &str) -> Option<Scanned> {
    let open = format!("<interface name=\"{iface}\">");
    let start = xml.find(&open)? + open.len();
    let body = &xml[start..];
    let end = body.find("</interface>")?;
    let body = &body[..end];

    let mut methods = Vec::new();
    let mut cur: Option<(String, Vec<String>)> = None;
    let mut properties = BTreeSet::new();

    for line in body.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("<method name=\"") {
            if let Some(entry) = cur.take() {
                methods.push(entry);
            }
            let name = rest.split('"').next().unwrap_or_default().to_string();
            cur = Some((name, Vec::new()));
        } else if let Some(rest) = t.strip_prefix("<property name=\"") {
            if let Some(entry) = cur.take() {
                methods.push(entry);
            }
            properties.insert(rest.split('"').next().unwrap_or_default().to_string());
        } else if let Some(rest) = t.strip_prefix("<signal name=\"") {
            if let Some(entry) = cur.take() {
                methods.push(entry);
            }
            let _ = rest;
        } else if t.starts_with("<arg") {
            // Skip the standard-interface boilerplate.
            if let (Some(args), Some(ty)) = (cur.as_mut(), t.split("type=\"").nth(1)) {
                args.1
                    .push(ty.split('"').next().unwrap_or_default().to_string());
            }
        } else if (t.starts_with("</method") || t.starts_with("</signal"))
            && let Some(entry) = cur.take()
        {
            methods.push(entry);
        }
    }
    if let Some(entry) = cur.take() {
        methods.push(entry);
    }

    Some(Scanned {
        methods,
        properties,
    })
}

async fn introspect_backend() -> Option<String> {
    use zbus::fdo::IntrospectableProxy;

    let Ok(b) = zbus::connection::Builder::session() else {
        return None;
    };
    let Ok(server_conn) = b.build().await else {
        return None;
    };
    let Ok(cb) = zbus::connection::Builder::session() else {
        return None;
    };
    let Ok(client) = cb.build().await else {
        return None;
    };

    let proxy = UiProxy {
        context: gtk4::glib::MainContext::default(),
        sender: tokio::sync::mpsc::unbounded_channel().0,
    };
    let sm = || {
        xdg_desktop_portal_gtk4::core::session_manager::SessionManager::new(server_conn.clone(), 10)
    };
    let server = server_conn.object_server().clone();
    macro_rules! at {
        ($e:expr) => {
            server
                .at("/org/freedesktop/portal/desktop", $e)
                .await
                .expect("register portal object")
        };
    }

    at!(xdg_desktop_portal_gtk4::portals::file_chooser::dbus::FileChooser::new(&proxy, sm()));
    at!(xdg_desktop_portal_gtk4::portals::email::dbus::Email::new(
        sm()
    ));
    at!(xdg_desktop_portal_gtk4::portals::access::dbus::Access::new(
        &proxy,
        sm()
    ));
    at!(xdg_desktop_portal_gtk4::portals::account::dbus::Account::new(&proxy, sm()));
    at!(
        xdg_desktop_portal_gtk4::portals::notification::dbus::Notification::new(Some(
            server_conn.clone()
        ))
        .await
    );
    at!(
        xdg_desktop_portal_gtk4::portals::dynamic_launcher::dbus::DynamicLauncher::new(
            &proxy,
            sm()
        )
    );
    at!(xdg_desktop_portal_gtk4::portals::print::dbus::Print::new(
        &proxy,
        sm()
    ));
    at!(xdg_desktop_portal_gtk4::portals::inhibit::dbus::Inhibit::new(sm(), None).await);
    at!(
        xdg_desktop_portal_gtk4::portals::settings::dbus::SettingsPortal::new(
            &proxy,
            server.clone()
        )
    );
    at!(xdg_desktop_portal_gtk4::portals::lockdown::dbus::LockdownPortal::new());
    at!(xdg_desktop_portal_gtk4::portals::app_chooser::dbus::AppChooser::new(&proxy, sm()));
    at!(xdg_desktop_portal_gtk4::portals::usb::dbus::UsbPortal::new(
        &proxy,
        sm()
    ));
    at!(
        xdg_desktop_portal_gtk4::portals::clipboard::dbus::ClipboardPortal::new(
            server_conn.clone(),
            proxy,
            sm()
        )
    );

    let dest = server_conn.unique_name().unwrap().clone();
    let ip = IntrospectableProxy::builder(&client)
        .destination(dest)
        .map_err(|_| ())
        .expect("destination")
        .path("/org/freedesktop/portal/desktop")
        .map_err(|_| ())
        .expect("path")
        .build()
        .await
        .expect("build introspectable proxy");

    tokio::time::timeout(std::time::Duration::from_secs(30), ip.introspect())
        .await
        .expect("Introspect timed out")
        .ok()
}

/// Method signatures must match the upstream contract exactly. This is the
/// machine-checked version of the audit's "all method signatures and argument
/// orders are correct" claim — including the deliberately unusual
/// `Usb.AcquireDevices(handle, parent_window, app_id, …)` ordering.
#[tokio::test]
async fn method_signatures_match_upstream_contract() {
    let Some(xml) = introspect_backend().await else {
        eprintln!("no private session bus; skipping");
        return;
    };

    let mut problems = Vec::new();
    for (iface, methods, _) in EXPECTED {
        let full = format!("{IMPL_PREFIX}{iface}");
        let Some(scan) = scan_interface(&xml, &full) else {
            problems.push(format!("{full}: not exported"));
            continue;
        };
        let actual: std::collections::BTreeMap<_, _> = scan.methods.into_iter().collect();
        for (name, want_args) in *methods {
            match actual.get(*name) {
                None => problems.push(format!("{full}.{name}: missing")),
                Some(got) if got != want_args => {
                    problems.push(format!(
                        "{full}.{name}: expected [{}] got [{}]",
                        want_args.join(","),
                        got.join(",")
                    ));
                }
                Some(_) => {}
            }
        }
        // Anything exported that the contract does not declare.
        for name in actual.keys() {
            if !methods.iter().any(|(n, _)| n == name) {
                problems.push(format!("{full}.{name}: not in upstream contract"));
            }
        }
    }

    assert!(
        problems.is_empty(),
        "D-Bus method signature drift vs org.freedesktop.impl.portal.*.xml:\n  {}",
        problems.join("\n  ")
    );
}

/// Property names are case-sensitive. `#[zbus(property)]` derives the wire name
/// from the Rust fn name and capitalises it, so a getter written
/// `fn version()` is exported as `Version`, not `version`.
///
/// `clipboard/dbus.rs` and `settings/dbus.rs` used to omit the explicit
/// `name = "version"` and therefore exported `Version`, which the contract does
/// not define. This test was added with that defect present and caught it.
#[tokio::test]
async fn property_names_match_upstream_contract() {
    let Some(xml) = introspect_backend().await else {
        eprintln!("no private session bus; skipping");
        return;
    };

    let mut problems = Vec::new();
    for (iface, _, properties) in EXPECTED {
        let full = format!("{IMPL_PREFIX}{iface}");
        let Some(scan) = scan_interface(&xml, &full) else {
            problems.push(format!("{full}: not exported"));
            continue;
        };
        for want in *properties {
            if !scan.properties.contains(*want) {
                let got: Vec<_> = scan.properties.iter().cloned().collect();
                problems.push(format!("{full}: missing property `{want}` (has {got:?})"));
            }
        }
    }

    assert!(
        problems.is_empty(),
        "D-Bus property name drift:\n  {}",
        problems.join("\n  ")
    );
}

/// `data/gtk4.portal` advertises the interface set. If it names an interface
/// the binary does not export, `xdg-desktop-portal` will call it and get
/// `UnknownMethod`; if it omits one, the backend is silently unusable for it.
#[test]
fn portal_file_matches_exported_interfaces() {
    let manifest = include_str!("../data/gtk4.portal");
    let advertised: BTreeSet<String> = manifest
        .lines()
        .find_map(|l| l.strip_prefix("Interfaces="))
        .expect("Interfaces= line")
        .split(';')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    let expected: BTreeSet<String> = EXPECTED
        .iter()
        .map(|(i, _, _)| format!("{IMPL_PREFIX}{i}"))
        .collect();

    let missing_from_manifest: Vec<_> = expected
        .difference(&advertised)
        .cloned()
        .collect::<Vec<String>>();
    let missing_from_binary: Vec<_> = advertised
        .difference(&expected)
        .cloned()
        .collect::<Vec<_>>();

    assert!(
        missing_from_manifest.is_empty() && missing_from_binary.is_empty(),
        "data/gtk4.portal disagrees with the binary.\n  exported but not advertised: {missing_from_manifest:?}\n  advertised but not exported: {missing_from_binary:?}"
    );
}
