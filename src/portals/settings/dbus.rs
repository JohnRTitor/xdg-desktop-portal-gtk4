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
        let state = self.aggregator.read();
        let mut result = HashMap::new();

        let supported_namespaces = vec![
            NS_FREEDESKTOP_APPEARANCE.to_owned(),
            NS_GNOME_DESKTOP_INTERFACE.to_owned(),
            crate::portals::settings::aggregator::NS_KDE_KDEGLOBALS.to_owned(),
        ];

        let mut active_namespaces = Vec::new();
        if namespaces.is_empty() || namespaces.contains(&String::from("")) {
            active_namespaces = supported_namespaces.clone();
        } else {
            for requested_ns in namespaces {
                if requested_ns.ends_with('*') {
                    let prefix = requested_ns.trim_end_matches('*');
                    for available_ns in &supported_namespaces {
                        if available_ns.starts_with(prefix)
                            && !active_namespaces.contains(available_ns)
                        {
                            active_namespaces.push(available_ns.clone());
                        }
                    }
                } else if supported_namespaces.contains(&requested_ns)
                    && !active_namespaces.contains(&requested_ns)
                {
                    active_namespaces.push(requested_ns);
                }
            }
        }

        for ns in active_namespaces {
            if let Some(ns_map) = state.namespaces.get(&ns) {
                result.insert(ns.clone(), ns_map.clone());
            }
        }

        Ok(result)
    }

    #[zbus(signal)]
    async fn setting_changed(
        ctx: &SignalEmitter<'_>,
        namespace: &str,
        key: &str,
        value: &Value<'_>,
    ) -> zbus::Result<()>;

    #[zbus(property)]
    fn version(&self) -> u32 {
        2 // Version 2 introduced ReadAll
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
