use {
    std::collections::HashMap,
    zbus::{
        interface,
        zvariant::{OwnedValue, Value},
    },
    gtk4::gio::{prelude::SettingsExt, Settings, SettingsSchemaSource},
    zbus::object_server::SignalEmitter,
};

pub struct SettingsPortal {}

impl SettingsPortal {
    pub fn new() -> Self {
        Self {}
    }

    fn get_gnome_interface(&self) -> Option<Settings> {
        let source = SettingsSchemaSource::default()?;
        if source.lookup("org.gnome.desktop.interface", true).is_some() {
            Some(Settings::new("org.gnome.desktop.interface"))
        } else {
            None
        }
    }

    fn read_setting(&self, namespace: &str, key: &str) -> Option<OwnedValue> {
        if namespace == "org.freedesktop.appearance" {
            if key == "color-scheme" {
                if let Some(settings) = self.get_gnome_interface() {
                    let val: String = settings.string("color-scheme").into();
                    let scheme = map_color_scheme(val.as_str());
                    return OwnedValue::try_from(Value::U32(scheme)).ok();
                }
            } else if key == "contrast" {
                if let Some(settings) = self.get_gnome_interface() {
                    if let Some(schema) = settings.settings_schema() {
                        if schema.has_key("high-contrast") {
                            let high_contrast = settings.boolean("high-contrast");
                            let contrast = if high_contrast { 1u32 } else { 0u32 };
                            return OwnedValue::try_from(Value::U32(contrast)).ok();
                        }
                    }
                }
            }
        } else if namespace == "org.gnome.desktop.interface" {
            if let Some(settings) = self.get_gnome_interface() {
                if let Some(schema) = settings.settings_schema() {
                    if schema.has_key(key) {
                        let val = settings.value(key);
                        let type_string = val.type_().as_str();
                        if type_string == "s" {
                            if let Some(s) = val.get::<String>() {
                                return OwnedValue::try_from(Value::Str(s.into())).ok();
                            }
                        } else if type_string == "b" {
                            if let Some(b) = val.get::<bool>() {
                                return OwnedValue::try_from(Value::Bool(b)).ok();
                            }
                        } else if type_string == "u" {
                            if let Some(u) = val.get::<u32>() {
                                return OwnedValue::try_from(Value::U32(u)).ok();
                            }
                        } else if type_string == "i" {
                            if let Some(i) = val.get::<i32>() {
                                return OwnedValue::try_from(Value::I32(i)).ok();
                            }
                        } else if type_string == "d" {
                            if let Some(d) = val.get::<f64>() {
                                return OwnedValue::try_from(Value::F64(d)).ok();
                            }
                        }
                    }
                }
            }
        }
        None
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
    async fn read(&self, namespace: String, key: String) -> Result<OwnedValue, zbus::fdo::Error> {
        if let Some(val) = self.read_setting(&namespace, &key) {
            Ok(val)
        } else {
            Err(zbus::fdo::Error::Failed("Setting not found".to_string()))
        }
    }

    async fn read_all(
        &self,
        namespaces: Vec<String>,
    ) -> Result<HashMap<String, HashMap<String, OwnedValue>>, zbus::fdo::Error> {
        let mut result = HashMap::new();
        
        let all_namespaces = if namespaces.is_empty() {
            vec!["org.freedesktop.appearance".to_string(), "org.gnome.desktop.interface".to_string()]
        } else {
            namespaces
        };

        for ns in all_namespaces {
            let mut ns_map = HashMap::new();
            if ns == "org.freedesktop.appearance" {
                if let Some(val) = self.read_setting(&ns, "color-scheme") {
                    ns_map.insert("color-scheme".to_string(), val);
                }
                if let Some(val) = self.read_setting(&ns, "contrast") {
                    ns_map.insert("contrast".to_string(), val);
                }
            } else if ns == "org.gnome.desktop.interface" {
                if let Some(settings) = self.get_gnome_interface() {
                    if let Some(schema) = settings.settings_schema() {
                        for key in schema.list_keys() {
                            let key_str = key.as_str();
                            if let Some(val) = self.read_setting(&ns, key_str) {
                                ns_map.insert(key_str.to_string(), val);
                            }
                        }
                    }
                }
            }
            if !ns_map.is_empty() {
                result.insert(ns, ns_map);
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

    #[zbus(property, name = "version")]
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
