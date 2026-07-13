use {
    crate::portal::{request::run_request, response::Response},
    std::collections::HashMap,
    zbus::{
        interface,
        zvariant::{OwnedValue, Value},
        ObjectServer,
    },
    gtk4::gio::{Settings, SettingsSchemaSource},
};

pub struct SettingsPortal {
    gnome_interface: Option<Settings>,
}

impl SettingsPortal {
    pub fn new() -> Self {
        let source = SettingsSchemaSource::default();
        let gnome_interface = if let Some(source) = source {
            if source.lookup("org.gnome.desktop.interface", true).is_some() {
                Some(Settings::new("org.gnome.desktop.interface"))
            } else {
                None
            }
        } else {
            None
        };

        Self { gnome_interface }
    }

    fn read_setting(&self, namespace: &str, key: &str) -> Option<OwnedValue> {
        if namespace == "org.freedesktop.appearance" {
            if key == "color-scheme" {
                if let Some(ref settings) = self.gnome_interface {
                    let val: String = settings.string("color-scheme").into();
                    let scheme = match val.as_str() {
                        "prefer-dark" => 1u32,
                        "prefer-light" => 2u32,
                        _ => 0u32,
                    };
                    return Some(Value::U32(scheme).into());
                }
            } else if key == "contrast" {
                if let Some(ref settings) = self.gnome_interface {
                    if settings.has_key("high-contrast") {
                        let high_contrast = settings.boolean("high-contrast");
                        let contrast = if high_contrast { 1u32 } else { 0u32 };
                        return Some(Value::U32(contrast).into());
                    }
                }
            }
        } else if namespace == "org.gnome.desktop.interface" {
            if let Some(ref settings) = self.gnome_interface {
                if settings.has_key(key) {
                    if let Some(val) = settings.value(key) {
                        // convert glib::Variant to zbus::zvariant::OwnedValue
                        // Since this is complex to do generically, we'll implement a few common ones
                        let type_string = val.type_().as_str();
                        if type_string == "s" {
                            if let Some(s) = val.get::<String>() {
                                return Some(Value::Str(s.into()).into());
                            }
                        } else if type_string == "b" {
                            if let Some(b) = val.get::<bool>() {
                                return Some(Value::Bool(b).into());
                            }
                        } else if type_string == "u" {
                            if let Some(u) = val.get::<u32>() {
                                return Some(Value::U32(u).into());
                            }
                        } else if type_string == "i" {
                            if let Some(i) = val.get::<i32>() {
                                return Some(Value::I32(i).into());
                            }
                        } else if type_string == "d" {
                            if let Some(d) = val.get::<f64>() {
                                return Some(Value::F64(d).into());
                            }
                        }
                    }
                }
            }
        }
        None
    }
}

#[interface(name = "org.freedesktop.impl.portal.Settings")]
impl SettingsPortal {
    async fn read(&self, namespace: String, key: String) -> zbus::Result<OwnedValue> {
        if let Some(val) = self.read_setting(&namespace, &key) {
            Ok(val)
        } else {
            Err(zbus::Error::Failure("Setting not found".to_string()))
        }
    }

    async fn read_all(
        &self,
        namespaces: Vec<String>,
    ) -> zbus::Result<HashMap<String, HashMap<String, OwnedValue>>> {
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
                if let Some(ref settings) = self.gnome_interface {
                    for key in settings.list_keys() {
                        if let Some(val) = self.read_setting(&ns, &key) {
                            ns_map.insert(key.to_string(), val);
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
        ctx: &zbus::SignalContext<'_>,
        namespace: &str,
        key: &str,
        value: &Value<'_>,
    ) -> zbus::Result<()>;
}
