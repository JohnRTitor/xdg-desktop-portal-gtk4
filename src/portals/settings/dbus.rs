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
    portals::settings::aggregator::{SettingsAggregator, SettingsState},
};

const NS_FREEDESKTOP_APPEARANCE: &str = "org.freedesktop.appearance";
const NS_GNOME_DESKTOP_INTERFACE: &str = "org.gnome.desktop.interface";

/// The namespaces this portal can serve, in the order the contract expects them.
const SUPPORTED_NAMESPACES: [&str; 3] = [
    NS_FREEDESKTOP_APPEARANCE,
    NS_GNOME_DESKTOP_INTERFACE,
    crate::portals::settings::aggregator::NS_KDE_KDEGLOBALS,
];

/// Resolves the caller's requested namespace list against the supported set.
///
/// A request for an empty list, or one containing `""`, means "everything".
/// Otherwise each entry is either an exact namespace or a `prefix*` wildcard.
/// Duplicates are dropped, and a wildcard may not re-add a namespace that was
/// already matched exactly.
///
/// Returns borrowed slices out of [`SUPPORTED_NAMESPACES`] — the returned
/// `Vec` borrows from `SUPPORTED_NAMESPACES`, never from `requested`, so an
/// unsupported namespace contributes no allocation at all.
fn select_namespaces(requested: &[String]) -> Vec<&'static str> {
    if requested.is_empty() || requested.iter().any(|ns| ns.is_empty()) {
        return SUPPORTED_NAMESPACES.to_vec();
    }

    let mut active: Vec<&'static str> = Vec::with_capacity(SUPPORTED_NAMESPACES.len());
    for requested_ns in requested {
        // `trim_end_matches`, not `strip_suffix`: the previous implementation
        // trimmed every trailing `*`, so `"org.*.*"` and `"**"` keep matching the
        // same prefixes they always did.
        if requested_ns.ends_with('*') {
            let prefix = requested_ns.trim_end_matches('*');
            for available_ns in SUPPORTED_NAMESPACES {
                if available_ns.starts_with(prefix) && !active.contains(&available_ns) {
                    active.push(available_ns);
                }
            }
        } else if let Some(available_ns) =
            SUPPORTED_NAMESPACES.iter().find(|ns| **ns == requested_ns)
            && !active.contains(available_ns)
        {
            active.push(available_ns);
        }
    }
    active
}

/// Builds the `ReadAll` reply for the caller's requested namespaces.
///
/// Only the namespaces the caller asked for (or all of them) appear, and each is
/// accompanied by a copy of its key/value map: the reply type is an owned
/// `HashMap<String, HashMap<String, OwnedValue>>`, so the inner copy is required
/// by the D-Bus contract rather than by the lookup.
fn read_all_from_state(
    state: &SettingsState,
    requested: &[String],
) -> HashMap<String, HashMap<String, OwnedValue>> {
    let mut result = HashMap::new();
    for ns in select_namespaces(requested) {
        if let Some(ns_map) = state.namespaces.get(ns) {
            result.insert(ns.to_owned(), ns_map.clone());
        }
    }
    result
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
    use super::*;

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

    #[test]
    fn select_empty_request_returns_everything() {
        assert_eq!(select_namespaces(&[]), SUPPORTED_NAMESPACES);
    }

    #[test]
    fn select_empty_string_means_everything() {
        // The contract treats an empty namespace entry as "all namespaces".
        assert_eq!(select_namespaces(&ns(&[""])), SUPPORTED_NAMESPACES);
    }

    #[test]
    fn select_exact_namespaces_in_requested_order() {
        assert_eq!(
            select_namespaces(&ns(&[
                NS_GNOME_DESKTOP_INTERFACE,
                NS_FREEDESKTOP_APPEARANCE
            ])),
            vec![NS_GNOME_DESKTOP_INTERFACE, NS_FREEDESKTOP_APPEARANCE],
        );
    }

    #[test]
    fn select_drops_duplicates() {
        assert_eq!(
            select_namespaces(&ns(&[NS_FREEDESKTOP_APPEARANCE, NS_FREEDESKTOP_APPEARANCE])),
            vec![NS_FREEDESKTOP_APPEARANCE],
        );
    }

    #[test]
    fn select_wildcard_expands_to_matching_namespaces() {
        // `org.freedesktop.*` matches only the appearance namespace.
        assert_eq!(
            select_namespaces(&ns(&["org.freedesktop.*"])),
            vec![NS_FREEDESKTOP_APPEARANCE],
        );
        assert_eq!(
            select_namespaces(&ns(&["org.gnome.desktop.*"])),
            vec![NS_GNOME_DESKTOP_INTERFACE],
        );
        assert_eq!(
            select_namespaces(&ns(&["org.*"])),
            vec![
                NS_FREEDESKTOP_APPEARANCE,
                NS_GNOME_DESKTOP_INTERFACE,
                crate::portals::settings::aggregator::NS_KDE_KDEGLOBALS
            ],
        );
    }

    #[test]
    fn select_wildcard_does_not_duplicate_an_exact_match() {
        // The wildcard branch must not re-add a namespace already matched.
        assert_eq!(
            select_namespaces(&ns(&[NS_FREEDESKTOP_APPEARANCE, "org.freedesktop.*",])),
            vec![NS_FREEDESKTOP_APPEARANCE],
        );
    }

    #[test]
    fn select_unknown_namespaces_are_dropped() {
        assert!(select_namespaces(&ns(&["com.example.Nope"])).is_empty());
        assert!(select_namespaces(&ns(&["com.example.*"])).is_empty());
    }

    #[test]
    fn select_lone_star_matches_everything() {
        assert_eq!(select_namespaces(&ns(&["*"])), SUPPORTED_NAMESPACES);
    }

    #[test]
    fn select_trims_every_trailing_star() {
        // `trim_end_matches` strips every *consecutive* trailing `*`, which is
        // what the previous implementation did. `"**"` therefore means "no
        // prefix" and matches everything.
        assert_eq!(select_namespaces(&ns(&["**"])), SUPPORTED_NAMESPACES);
        assert_eq!(
            select_namespaces(&ns(&["org.freedesktop.**"])),
            vec![NS_FREEDESKTOP_APPEARANCE],
        );
        assert_eq!(
            select_namespaces(&ns(&["org.gnome.desktop.**"])),
            vec![NS_GNOME_DESKTOP_INTERFACE],
        );
        // A `.` before the stars stops the trim, so this is a literal prefix and
        // matches nothing -- preserved from the previous behaviour.
        assert!(select_namespaces(&ns(&["org.gnome.desktop.*.*"])).is_empty());
    }

    #[test]
    fn select_namespaces_allocates_only_the_result_vec() {
        // The resolved list borrows from `SUPPORTED_NAMESPACES`, so it costs
        // exactly one `Vec` allocation regardless of how many entries are
        // requested. Previously every request allocated a `String` per
        // supported namespace, plus a clone per match.
        for requested in [
            vec![],
            ns(&[""]),
            ns(&["*"]),
            ns(&["org.*"]),
            ns(&[NS_FREEDESKTOP_APPEARANCE, "org.*"]),
        ] {
            let scope = crate::alloc_probe::AllocScope::start();
            let selected = select_namespaces(&requested);
            let snap = scope.finish();
            std::hint::black_box(&selected);
            assert_eq!(
                snap.count, 1,
                "expected only the result Vec to allocate, got {snap:?} for {requested:?}",
            );
        }
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

    fn value(s: &str) -> OwnedValue {
        OwnedValue::try_from(Value::from(s)).expect("str to OwnedValue is infallible")
    }

    /// Populates all three namespaces the aggregator can produce.
    fn populated() -> SettingsState {
        let mut state = SettingsState::default();
        state.insert(NS_FREEDESKTOP_APPEARANCE, "color-scheme", value("1"));
        state.insert(GNOME, "gtk-theme-name", value("Adwaita"));
        state.insert(KDE, "widgetStyle", value("Breeze"));
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
        let mut expected = vec![GNOME, KDE, NS_FREEDESKTOP_APPEARANCE];
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
    fn a_requested_but_unpopulated_namespace_is_omitted() {
        // `select_namespaces` resolves against the *supported* list, not what
        // happens to be loaded.
        let mut state = SettingsState::default();
        state.insert(GNOME, "gtk-theme-name", value("Adwaita"));
        let result = read_all_from_state(&state, &[]);
        assert_eq!(keys(&result), vec![GNOME]);
    }

    #[test]
    fn wildcard_returns_only_the_matching_namespaces() {
        let result = read_all_from_state(&populated(), &["org.freedesktop.*".to_owned()]);
        assert_eq!(keys(&result), vec![NS_FREEDESKTOP_APPEARANCE]);
    }

    #[test]
    fn values_survive_the_round_trip() {
        let result = read_all_from_state(&populated(), &[KDE.to_owned()]);
        assert_eq!(result[KDE]["widgetStyle"], value("Breeze"));
    }
}
