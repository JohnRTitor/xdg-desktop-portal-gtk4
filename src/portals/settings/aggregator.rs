use {
    gtk4::{
        gio::{Settings, SettingsSchemaSource, prelude::SettingsExt},
        glib::{self, KeyFile, KeyFileFlags},
    },
    parking_lot::RwLock,
    std::{collections::HashMap, sync::Arc},
    zbus::zvariant::{OwnedValue, Value},
};

use crate::portals::settings::dbus::map_color_scheme;

pub const NS_FREEDESKTOP_APPEARANCE: &str = "org.freedesktop.appearance";
pub const NS_GNOME_DESKTOP_INTERFACE: &str = "org.gnome.desktop.interface";
pub const NS_KDE_KDEGLOBALS: &str = "org.kde.kdeglobals";
pub const KEY_COLOR_SCHEME: &str = "color-scheme";

#[derive(Default, Debug)]
pub struct SettingsState {
    pub namespaces: HashMap<String, HashMap<String, OwnedValue>>,
}

impl SettingsState {
    pub fn get(&self, ns: &str, key: &str) -> Option<OwnedValue> {
        self.namespaces.get(ns).and_then(|m| m.get(key).cloned())
    }

    pub fn insert(&mut self, ns: &str, key: &str, val: OwnedValue) {
        self.namespaces
            .entry(ns.to_owned())
            .or_default()
            .insert(key.to_owned(), val);
    }
}

pub struct SettingsAggregator {
    pub state: Arc<RwLock<SettingsState>>,
}

impl Default for SettingsAggregator {
    fn default() -> Self {
        Self::new()
    }
}

impl SettingsAggregator {
    pub fn new() -> Self {
        Self {
            state: Arc::new(RwLock::new(SettingsState::default())),
        }
    }

    pub fn reload_all(&mut self) -> Vec<(String, String, OwnedValue)> {
        let mut new_state = SettingsState::default();

        // 1. Read GSettings
        Self::read_gsettings(&mut new_state);

        // 2. Read GTK 3.0 / 4.0 settings.ini
        Self::read_gtk_settings_ini(&mut new_state, "gtk-3.0");
        Self::read_gtk_settings_ini(&mut new_state, "gtk-4.0");

        // 3. Read kdeglobals
        Self::read_kdeglobals(&mut new_state);

        // 4. Resolve org.freedesktop.appearance
        Self::resolve_appearance(&mut new_state);

        let mut changes = Vec::new();
        {
            let old_state = self.state.read();
            for (ns, keys) in &new_state.namespaces {
                for (key, new_val) in keys {
                    if let Some(old_val) = old_state.get(ns, key) {
                        if old_val != *new_val {
                            changes.push((ns.clone(), key.clone(), new_val.clone()));
                        }
                    } else {
                        changes.push((ns.clone(), key.clone(), new_val.clone()));
                    }
                }
            }
        }

        *self.state.write() = new_state;

        changes
    }

    fn read_gsettings(state: &mut SettingsState) {
        let Some(source) = SettingsSchemaSource::default() else {
            return;
        };
        if source.lookup(NS_GNOME_DESKTOP_INTERFACE, true).is_none() {
            return;
        }

        let settings = Settings::new(NS_GNOME_DESKTOP_INTERFACE);
        let Some(schema) = settings.settings_schema() else {
            return;
        };

        for key in schema.list_keys() {
            let key_str = key.as_str();
            let val = settings.value(key_str);
            let type_string = val.type_().as_str();
            let owned_val = match type_string {
                "s" => val
                    .get::<String>()
                    .and_then(|s| OwnedValue::try_from(Value::Str(s.into())).ok()),
                "b" => val
                    .get::<bool>()
                    .and_then(|b| OwnedValue::try_from(Value::Bool(b)).ok()),
                "u" => val
                    .get::<u32>()
                    .and_then(|u| OwnedValue::try_from(Value::U32(u)).ok()),
                "i" => val
                    .get::<i32>()
                    .and_then(|i| OwnedValue::try_from(Value::I32(i)).ok()),
                "d" => val
                    .get::<f64>()
                    .and_then(|d| OwnedValue::try_from(Value::F64(d)).ok()),
                _ => None,
            };
            let Some(v) = owned_val else {
                continue;
            };
            state.insert(NS_GNOME_DESKTOP_INTERFACE, key_str, v);
        }
    }

    fn read_gtk_settings_ini(state: &mut SettingsState, version: &str) {
        let mut config_path = glib::user_config_dir();
        config_path.push(version);
        config_path.push("settings.ini");

        let key_file = KeyFile::new();
        if key_file
            .load_from_file(&config_path, KeyFileFlags::NONE)
            .is_err()
        {
            return;
        }

        let Ok(keys) = key_file.keys("Settings") else {
            return;
        };

        for key in keys {
            let key_str = key.as_str();
            let Ok(val) = key_file.value("Settings", key_str) else {
                continue;
            };

            let val_str = val.as_str();
            let owned = if val_str == "true" {
                OwnedValue::try_from(Value::Bool(true))
                    .expect("Converting primitive Value to OwnedValue is infallible")
            } else if val_str == "false" {
                OwnedValue::try_from(Value::Bool(false))
                    .expect("Converting primitive Value to OwnedValue is infallible")
            } else if let Ok(num) = val_str.parse::<i32>() {
                OwnedValue::try_from(Value::I32(num))
                    .expect("Converting primitive Value to OwnedValue is infallible")
            } else {
                OwnedValue::try_from(Value::Str(val_str.into()))
                    .expect("Converting primitive Value to OwnedValue is infallible")
            };
            state.insert(NS_GNOME_DESKTOP_INTERFACE, key_str, owned);
        }
    }

    fn read_kdeglobals(state: &mut SettingsState) {
        let mut config_path = glib::user_config_dir();
        config_path.push("kdeglobals");

        let key_file = KeyFile::new();
        if key_file
            .load_from_file(&config_path, KeyFileFlags::NONE)
            .is_err()
        {
            return;
        }

        let groups = key_file.groups();
        for group in groups {
            let group_str = group.as_str();
            let ns = format!("{}.{}", NS_KDE_KDEGLOBALS, group_str);
            let Ok(keys) = key_file.keys(group_str) else {
                continue;
            };

            for key in keys {
                let key_str = key.as_str();
                let Ok(val) = key_file.value(group_str, key_str) else {
                    continue;
                };

                let val_str = val.as_str();
                let owned = if val_str == "true" {
                    OwnedValue::try_from(Value::Bool(true))
                        .expect("Converting primitive Value to OwnedValue is infallible")
                } else if val_str == "false" {
                    OwnedValue::try_from(Value::Bool(false))
                        .expect("Converting primitive Value to OwnedValue is infallible")
                } else {
                    OwnedValue::try_from(Value::Str(val_str.into()))
                        .expect("Converting primitive Value to OwnedValue is infallible")
                };
                state.insert(&ns, key_str, owned);
            }
        }
    }

    fn resolve_appearance(state: &mut SettingsState) {
        let mut color_scheme_val = 0u32;
        if let Some(cs) = state.get(NS_GNOME_DESKTOP_INTERFACE, KEY_COLOR_SCHEME) {
            if let Value::Str(s) = &*cs {
                color_scheme_val = map_color_scheme(s.as_str());
            }
        } else if let Some(pref_dark) = state.get(
            NS_GNOME_DESKTOP_INTERFACE,
            "gtk-application-prefer-dark-theme",
        ) && let Value::Bool(b) = &*pref_dark
        {
            color_scheme_val = if *b { 1 } else { 2 };
        }
        state.insert(
            NS_FREEDESKTOP_APPEARANCE,
            KEY_COLOR_SCHEME,
            OwnedValue::try_from(Value::U32(color_scheme_val))
                .expect("Converting primitive Value to OwnedValue is infallible"),
        );

        let mut contrast_val = 0u32;
        if let Some(hc) = state.get(NS_GNOME_DESKTOP_INTERFACE, "gtk-theme-name")
            && let Value::Str(s) = &*hc
            && s.as_str().contains("HighContrast")
        {
            contrast_val = 1;
        }
        state.insert(
            NS_FREEDESKTOP_APPEARANCE,
            "contrast",
            OwnedValue::try_from(Value::U32(contrast_val))
                .expect("Converting primitive Value to OwnedValue is infallible"),
        );

        let mut reduced_motion_val = 0u32;
        if let Some(ea) = state.get(NS_GNOME_DESKTOP_INTERFACE, "gtk-enable-animations")
            && let Value::Bool(b) = &*ea
        {
            reduced_motion_val = if *b { 0 } else { 1 };
        }
        state.insert(
            NS_FREEDESKTOP_APPEARANCE,
            "reduced-motion",
            OwnedValue::try_from(Value::U32(reduced_motion_val))
                .expect("Converting primitive Value to OwnedValue is infallible"),
        );
    }
}
