use {
    gtk4::gio::{Settings, SettingsSchemaSource, prelude::SettingsExt},
    parking_lot::RwLock,
    std::{collections::HashMap, sync::Arc, time::Duration},
    zbus::{
        fdo, interface,
        object_server::SignalEmitter,
        zvariant::{OwnedValue, Value},
    },
};

use crate::{
    gui::UiProxy,
    portals::settings::aggregator::{
        NS_GNOME_DESKTOP_INTERFACE, SettingsAggregator, SettingsState,
    },
};

#[cfg(test)]
use crate::portals::settings::aggregator::NS_FREEDESKTOP_APPEARANCE;

/// Does `namespace` satisfy any of the caller's `patterns`?
///
/// This is the namespace matcher the portal contract describes, applied to each
/// namespace the backend actually has data for rather than to a hardcoded
/// support list. A pattern matches when it is the empty string (meaning "all"),
/// an exact match, or a prefix when it ends in `*`.
///
/// Matching against the live state rather than a fixed list is what makes
/// `Read` and `ReadAll` agree. `read_kdeglobals` publishes one namespace per
/// INI group, `org.kde.kdeglobals.<Group>`, so a support list containing only the
/// bare `org.kde.kdeglobals` could never select any of them and `ReadAll` would
/// silently omit data that a direct `Read` of the same namespace returned. An
/// exact-match lookup against the real keys closes that gap without needing to
/// enumerate group names up front.
fn namespace_matches(namespace: &str, patterns: &[String]) -> bool {
    if patterns.is_empty() {
        return true;
    }
    patterns.iter().any(|pattern| {
        if pattern.is_empty() {
            return true;
        }
        if pattern == namespace {
            return true;
        }
        // Strip exactly one trailing `*`, matching the reference: a pattern of
        // `org.*` is a prefix match, while `org.*.*` is not a valid pattern and
        // must not degenerate into "everything".
        pattern
            .strip_suffix('*')
            .is_some_and(|prefix| namespace.starts_with(prefix))
    })
}

/// Builds the `ReadAll` reply for the caller's requested namespaces.
///
/// Only the namespaces the caller asked for (or all of them) appear, and each is
/// accompanied by a copy of its key/value map: the reply type is an owned
/// `HashMap<String, HashMap<String, OwnedValue>>`, so the inner copy is required
/// by the D-Bus contract rather than by the lookup.
///
/// A namespace that is supported but has no data is omitted rather than sent as
/// an empty map. That matches every reference backend, and it keeps `ReadAll`
/// consistent with `Read`, which reports "not found" for such a namespace.
fn read_all_from_state(
    state: &SettingsState,
    requested: &[String],
) -> HashMap<String, HashMap<String, OwnedValue>> {
    state
        .namespaces
        .iter()
        .filter(|(ns, _)| namespace_matches(ns, requested))
        .map(|(ns, keys)| (ns.clone(), keys.clone()))
        .collect()
}

/// D-Bus interface wrapper for the Settings portal.
///
/// This portal requires no active UI; it simply reads keys from the underlying
/// GTK/GLib settings store. It actively listens to `GSettings` changes and
/// broadcasts them over D-Bus as `SettingChanged` signals.
pub struct SettingsPortal {
    pub aggregator: Arc<RwLock<SettingsState>>,
}

impl SettingsPortal {
    pub fn new(proxy: &UiProxy, server: zbus::ObjectServer) -> Self {
        let mut agg = SettingsAggregator::new();
        let state = agg.state.clone();
        let sender = proxy.sender.clone();

        tokio::spawn(async move {
            use {
                gtk4::glib,
                notify::{RecursiveMode, Watcher},
                tokio::sync::mpsc,
            };

            let (tx, mut rx) = mpsc::channel::<()>(100);

            // Watch GSettings on the GTK main thread.
            // `Settings` is a GObject (`!Send`), so we create it and connect the
            // change signal via UiProxy, which dispatches to the main thread.
            // The object is intentionally leaked to keep the signal handler alive
            // for the daemon's entire lifetime.
            {
                let tx_gsettings = tx.clone();
                let _ = sender.send(Box::new(move || {
                    let Some(source) = SettingsSchemaSource::default() else {
                        return;
                    };
                    if source.lookup(NS_GNOME_DESKTOP_INTERFACE, true).is_none() {
                        return;
                    }

                    let settings = Settings::new(NS_GNOME_DESKTOP_INTERFACE);
                    settings.connect_changed(None, move |_, _| {
                        let _ = tx_gsettings.try_send(());
                    });
                    // Prevent the Rust destructor (`g_object_unref`) from running.
                    // The daemon is a long-running process and this Settings object
                    // must outlive the signal callback; leaking it is deliberate.
                    std::mem::forget(settings);
                }));
            }

            // Watch INI files — `notify` is fully thread-safe, runs fine in tokio.
            let tx_files = tx.clone();
            let _watcher =
                notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
                    if res.is_ok() {
                        let _ = tx_files.try_send(());
                    }
                })
                .ok()
                .map(|mut w| {
                    let config_dir = glib::user_config_dir();
                    let _ = w.watch(&config_dir.join("gtk-3.0"), RecursiveMode::NonRecursive);
                    let _ = w.watch(&config_dir.join("gtk-4.0"), RecursiveMode::NonRecursive);
                    let _ = w.watch(&config_dir.join("kdeglobals"), RecursiveMode::NonRecursive);
                    w
                });

            // Initial load
            agg.reload_all();

            // Event loop: react to change notifications and emit D-Bus signals.
            while let Some(()) = rx.recv().await {
                // Debounce: coalesce rapid-fire events into a single reload.
                tokio::time::sleep(Duration::from_millis(50)).await;
                while rx.try_recv().is_ok() {}

                let changes = agg.reload_all();
                if changes.is_empty() {
                    continue;
                }

                let Ok(iface_ref) = server
                    .interface::<_, SettingsPortal>(crate::core::DBUS_PATH)
                    .await
                else {
                    continue;
                };

                for (ns, key, val) in changes {
                    let _ =
                        Self::setting_changed(iface_ref.signal_emitter(), &ns, &key, &val).await;
                }
            }
        });

        Self { aggregator: state }
    }
}

pub(crate) fn map_color_scheme(val: &str) -> u32 {
    match val {
        "prefer-dark" => 1u32,
        "prefer-light" => 2u32,
        _ => 0u32,
    }
}

#[interface(name = "org.freedesktop.impl.portal.Settings")]
impl SettingsPortal {
    async fn read(&self, namespace: String, key: String) -> Result<OwnedValue, fdo::Error> {
        let state = self.aggregator.read();
        if let Some(val) = state.get(&namespace, &key) {
            Ok(val)
        } else {
            Err(fdo::Error::Failed("Setting not found".into()))
        }
    }

    async fn read_all(
        &self,
        namespaces: Vec<String>,
    ) -> Result<HashMap<String, HashMap<String, OwnedValue>>, fdo::Error> {
        Ok(read_all_from_state(&self.aggregator.read(), &namespaces))
    }

    #[zbus(signal)]
    async fn setting_changed(
        ctx: &SignalEmitter<'_>,
        namespace: &str,
        key: &str,
        value: &Value<'_>,
    ) -> zbus::Result<()>;

    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        2 // Version 2 introduced ReadAll
    }
}

#[cfg(test)]
mod tests {
    use {super::*, crate::portals::settings::aggregator::NS_KDE_KDEGLOBALS};

    fn ns(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn test_color_scheme_prefer_dark() {
        assert_eq!(map_color_scheme("prefer-dark"), 1);
    }

    #[test]
    fn test_color_scheme_prefer_light() {
        assert_eq!(map_color_scheme("prefer-light"), 2);
    }

    #[test]
    fn test_color_scheme_default() {
        assert_eq!(map_color_scheme("default"), 0);
    }

    #[test]
    fn test_color_scheme_unknown() {
        assert_eq!(map_color_scheme("foobar"), 0);
    }

    // --- namespace matching -------------------------------------------------
    //
    // These pin the matching rules the contract states for `ReadAll`
    // (`org.freedesktop.impl.portal.Settings.xml`): an empty array or an empty
    // string matches everything, and globbing applies "only for trailing
    // sections", e.g. `org.example.*`.

    #[test]
    fn an_empty_pattern_list_matches_every_namespace() {
        for ns_name in [
            NS_FREEDESKTOP_APPEARANCE,
            NS_GNOME_DESKTOP_INTERFACE,
            NS_KDE_KDEGLOBALS,
            "com.example.Anything",
        ] {
            assert!(
                namespace_matches(ns_name, &[]),
                "an empty request must mean everything, but {ns_name} was excluded"
            );
        }
    }

    #[test]
    fn an_empty_string_pattern_means_everything() {
        assert!(namespace_matches(NS_FREEDESKTOP_APPEARANCE, &ns(&[""])));
        assert!(namespace_matches("com.example.Anything", &ns(&[""])));
    }

    #[test]
    fn an_exact_pattern_matches_only_itself() {
        let patterns = ns(&[NS_GNOME_DESKTOP_INTERFACE]);
        assert!(namespace_matches(NS_GNOME_DESKTOP_INTERFACE, &patterns));
        assert!(!namespace_matches(NS_FREEDESKTOP_APPEARANCE, &patterns));
    }

    #[test]
    fn a_trailing_star_is_a_prefix_match() {
        assert!(namespace_matches(
            NS_FREEDESKTOP_APPEARANCE,
            &ns(&["org.freedesktop.*"])
        ));
        assert!(namespace_matches(
            NS_GNOME_DESKTOP_INTERFACE,
            &ns(&["org.gnome.desktop.*"])
        ));
        for ns_name in [
            NS_FREEDESKTOP_APPEARANCE,
            NS_GNOME_DESKTOP_INTERFACE,
            NS_KDE_KDEGLOBALS,
        ] {
            assert!(
                namespace_matches(ns_name, &ns(&["org.*"])),
                "org.* must select {ns_name}"
            );
        }
    }

    #[test]
    fn a_bare_star_matches_everything() {
        for ns_name in [
            NS_FREEDESKTOP_APPEARANCE,
            NS_GNOME_DESKTOP_INTERFACE,
            "com.example.Anything",
        ] {
            assert!(
                namespace_matches(ns_name, &ns(&["*"])),
                "* missed {ns_name}"
            );
        }
    }

    #[test]
    fn an_unrelated_namespace_does_not_match() {
        let patterns = ns(&["com.example.Nope"]);
        assert!(!namespace_matches(NS_FREEDESKTOP_APPEARANCE, &patterns));
        assert!(!namespace_matches(NS_GNOME_DESKTOP_INTERFACE, &patterns));
    }

    /// Regression: the previous matcher used `trim_end_matches('*')`, which
    /// stripped *every* trailing star. `org.freedesktop.**` therefore collapsed
    /// to the prefix `org.freedesktop.` and matched, though it is not a valid
    /// pattern. The reference strips exactly one.
    #[test]
    fn only_one_trailing_star_is_consumed() {
        assert!(namespace_matches(
            NS_FREEDESKTOP_APPEARANCE,
            &ns(&["org.freedesktop.*"])
        ));
        assert!(
            !namespace_matches(NS_FREEDESKTOP_APPEARANCE, &ns(&["org.freedesktop.**"])),
            "a doubled star must not be treated as a prefix wildcard"
        );
        assert!(
            !namespace_matches(NS_FREEDESKTOP_APPEARANCE, &ns(&["**"])),
            "a bare doubled star must not match everything"
        );
    }

    /// Regression: a doubled star mid-pattern used to reduce the prefix to
    /// `org.gnome.desktop.` and match. One star is stripped, leaving a literal
    /// `*` at the front of the prefix, which no namespace can start with.
    #[test]
    fn a_star_in_the_middle_does_not_match() {
        assert!(!namespace_matches(
            NS_GNOME_DESKTOP_INTERFACE,
            &ns(&["org.gnome.desktop.*.*"])
        ));
    }

    /// Regression: `read_kdeglobals` publishes `org.kde.kdeglobals.<Group>`, so a
    /// bare `org.kde.kdeglobals` prefix must select those group namespaces. They
    /// were previously unreachable through `ReadAll`, because the matcher only
    /// knew the bare `org.kde.kdeglobals` name and never the group-suffixed ones.
    #[test]
    fn the_kde_prefix_selects_group_namespaces() {
        let group_ns = "org.kde.kdeglobals.KDE";
        assert!(namespace_matches(group_ns, &ns(&["org.kde.*"])));
        assert!(namespace_matches(group_ns, &ns(&["org.kde.kdeglobals.*"])));
        assert!(namespace_matches(group_ns, &ns(&[group_ns])));
        assert!(
            !namespace_matches(NS_GNOME_DESKTOP_INTERFACE, &ns(&["org.kde.*"])),
            "the KDE prefix must not leak neighbouring namespaces"
        );
    }

    #[test]
    fn matching_an_exact_group_name_needs_no_trailing_star() {
        assert!(namespace_matches(
            "org.kde.kdeglobals.KDE",
            &ns(&["org.kde.kdeglobals.KDE"])
        ));
    }
}

#[cfg(test)]
mod read_all_tests {
    use {
        super::*,
        crate::portals::settings::aggregator::{
            NS_GNOME_DESKTOP_INTERFACE as GNOME, NS_KDE_KDEGLOBALS as KDE, SettingsState,
        },
    };

    /// A namespace name with an INI group suffix, which is the exact shape
    /// `read_kdeglobals` writes: `format!("{}.{}", NS_KDE_KDEGLOBALS, group)`.
    const KDE_GROUP: &str = "org.kde.kdeglobals.KDE";

    fn value(s: &str) -> OwnedValue {
        OwnedValue::try_from(Value::from(s)).expect("str to OwnedValue is infallible")
    }

    /// Populates the namespaces the aggregator can produce, including the
    /// group-suffixed form `read_kdeglobals` actually publishes.
    fn populated() -> SettingsState {
        let mut state = SettingsState::default();
        state.insert(NS_FREEDESKTOP_APPEARANCE, "color-scheme", value("1"));
        state.insert(GNOME, "gtk-theme-name", value("Adwaita"));
        state.insert(KDE_GROUP, "widgetStyle", value("Breeze"));
        state
    }

    fn keys(result: &HashMap<String, HashMap<String, OwnedValue>>) -> Vec<&str> {
        let mut v: Vec<&str> = result.keys().map(String::as_str).collect();
        v.sort_unstable();
        v
    }

    #[test]
    fn a_single_requested_namespace_returns_only_that_namespace() {
        let result = read_all_from_state(&populated(), &[GNOME.to_owned()]);
        assert_eq!(keys(&result), vec![GNOME]);
        assert_eq!(result[GNOME]["gtk-theme-name"], value("Adwaita"));
    }

    #[test]
    fn an_unsupported_namespace_returns_nothing() {
        let result = read_all_from_state(&populated(), &["com.example.Nope".to_owned()]);
        assert!(result.is_empty());
    }

    #[test]
    fn an_unsupported_namespace_does_not_leak_its_neighbours() {
        let result = read_all_from_state(
            &populated(),
            &["com.example.Nope".to_owned(), GNOME.to_owned()],
        );
        assert_eq!(keys(&result), vec![GNOME]);
    }

    #[test]
    fn an_empty_request_returns_every_populated_namespace() {
        let mut expected = vec![GNOME, KDE_GROUP, NS_FREEDESKTOP_APPEARANCE];
        expected.sort_unstable();
        assert_eq!(keys(&read_all_from_state(&populated(), &[])), expected);
    }

    #[test]
    fn an_empty_string_means_every_namespace() {
        assert_eq!(
            keys(&read_all_from_state(&populated(), &["".to_owned()])),
            keys(&read_all_from_state(&populated(), &[])),
        );
    }

    #[test]
    fn a_requested_but_unloaded_namespace_is_omitted() {
        // Matching runs over the namespaces actually loaded, so a namespace the
        // backend knows about but has no data for simply does not appear. This
        // keeps `ReadAll` consistent with `Read`, which reports "not found" for
        // it rather than an empty map.
        let mut state = SettingsState::default();
        state.insert(GNOME, "gtk-theme-name", value("Adwaita"));
        let result = read_all_from_state(&state, &[]);
        assert_eq!(keys(&result), vec![GNOME]);
    }

    /// Regression: `read_kdeglobals` stores one namespace per INI group, as
    /// `org.kde.kdeglobals.<Group>`. The previous implementation resolved
    /// requests against a fixed list holding only the bare `org.kde.kdeglobals`,
    /// so none of the group namespaces it had written could ever be selected
    /// and every one of them was invisible to `ReadAll` -- while a direct `Read`
    /// of the same namespace returned data. These two calls must agree.
    #[test]
    fn group_suffixed_kde_namespaces_are_reachable() {
        let state = populated();

        // ReadAll with the exact name, and with the parent prefix.
        assert_eq!(
            keys(&read_all_from_state(&state, &[KDE_GROUP.to_owned()])),
            vec![KDE_GROUP]
        );
        assert_eq!(
            keys(&read_all_from_state(&state, &["org.kde.*".to_owned()])),
            vec![KDE_GROUP]
        );
        // The bare parent name is itself a prefix of the group name, and the
        // reference matcher treats a name without `*` as an exact comparison,
        // so it must NOT select the group.
        assert!(
            read_all_from_state(&state, &[KDE.to_owned()]).is_empty(),
            "the bare parent namespace is not a wildcard for its groups"
        );

        // The whole point: ReadAll must now find what Read finds.
        assert!(
            state.get(KDE_GROUP, "widgetStyle").is_some(),
            "test fixture is wrong: Read would have nothing to find"
        );
    }

    #[test]
    fn wildcard_returns_only_the_matching_namespaces() {
        let result = read_all_from_state(&populated(), &["org.freedesktop.*".to_owned()]);
        assert_eq!(keys(&result), vec![NS_FREEDESKTOP_APPEARANCE]);
    }

    #[test]
    fn values_survive_the_round_trip() {
        let result = read_all_from_state(&populated(), &[KDE_GROUP.to_owned()]);
        assert_eq!(result[KDE_GROUP]["widgetStyle"], value("Breeze"));
    }
}
