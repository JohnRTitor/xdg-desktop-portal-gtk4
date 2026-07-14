use std::collections::HashMap;
use zbus::zvariant::{SerializeDict, Type, OwnedObjectPath, OwnedValue};
use zbus::interface;
use crate::{
    gui::UiProxy,
    core::{request::run_request, response::Response},
};
use super::gui::{UsbUi, UsbError, UsbDevice};

#[derive(SerializeDict, Type, Debug, Default)]
#[zvariant(signature = "dict")]
pub struct UsbResults {
    devices: Vec<(String, HashMap<String, OwnedValue>)>,
}

pub struct UsbPortal {
    proxy: UiProxy,
}

impl UsbPortal {
    pub fn new(proxy: &UiProxy) -> Self {
        Self { proxy: proxy.clone() }
    }

    fn parse_udev_string(s: &str) -> String {
        s.replace("\\x20", " ")
    }

    fn extract_property(properties: &HashMap<String, OwnedValue>, keys: &[&str]) -> Option<String> {
        keys.iter().find_map(|&k| {
            properties.get(k).and_then(|val| {
                <&str>::try_from(val).ok().map(Self::parse_udev_string)
            })
        })
    }

    async fn acquire_devices_impl(
        &self,
        app_id: String,
        parent_window: String,
        devices_in: Vec<(String, HashMap<String, OwnedValue>, HashMap<String, OwnedValue>)>,
    ) -> Response<UsbResults> {
        let mut parsed_devices = Vec::new();
        for (id, props, access_options) in devices_in {
            // Find inner properties dict
            let mut properties = HashMap::new();
            if let Some(p) = props.get("properties") {
                if let Ok(dict) = <HashMap<String, OwnedValue>>::try_from(p.clone()) {
                    properties = dict;
                }
            } else {
                properties = props.clone();
            }

            let vendor = Self::extract_property(&properties, &["ID_VENDOR_FROM_DATABASE", "ID_VENDOR_ENC", "ID_VENDOR_ID"]);
            let model = Self::extract_property(&properties, &["ID_MODEL_FROM_DATABASE", "ID_MODEL_ENC", "ID_MODEL_ID"]);

            let mut serial = None;
            if let Some(val) = properties.get("ID_SERIAL_SHORT") {
                if let Ok(s) = <&str>::try_from(val) {
                    if !s.is_empty() {
                        serial = Some(Self::parse_udev_string(s));
                    }
                }
            }

            parsed_devices.push(UsbDevice {
                id,
                title: model.unwrap_or_else(|| rust_i18n::t!("Unknown device").to_string()),
                subtitle: vendor.unwrap_or_else(|| rust_i18n::t!("Unknown vendor").to_string()),
                serial,
                access_options,
            });
        }

        let ui = UsbUi {
            app_id,
            parent_window,
            devices: parsed_devices,
        };
        
        match ui.run(&self.proxy).await {
            Ok(result) => {
                let res = UsbResults {
                    devices: result.devices,
                };
                Response::success(res)
            }
            Err(UsbError::Closed) | Err(UsbError::Rejected) => {
                Response::cancelled()
            }
        }
    }
}

#[interface(name = "org.freedesktop.impl.portal.Usb")]
impl UsbPortal {
    #[zbus(name = "AcquireDevices")]
    async fn acquire_devices(
        &self,
        handle: OwnedObjectPath,
        parent_window: String,
        app_id: String,
        devices: Vec<(String, HashMap<String, OwnedValue>, HashMap<String, OwnedValue>)>,
        _options: HashMap<String, OwnedValue>,
        #[zbus(object_server)] server: &zbus::ObjectServer,
    ) -> Response<UsbResults> {
        run_request(
            server,
            handle,
            self.acquire_devices_impl(app_id, parent_window, devices)
        )
        .await
    }

    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zbus::zvariant::Type;

    #[test]
    fn test_parse_udev_basic() {
        assert_eq!(UsbPortal::parse_udev_string("Logitech\\x20Mouse"), "Logitech Mouse");
    }

    #[test]
    fn test_parse_udev_no_escape() {
        assert_eq!(UsbPortal::parse_udev_string("SimpleDevice"), "SimpleDevice");
    }

    #[test]
    fn test_parse_udev_multiple_escapes() {
        assert_eq!(UsbPortal::parse_udev_string("A\\x20B\\x20C"), "A B C");
    }

    #[test]
    fn test_usb_results_signature() {
        assert_eq!(UsbResults::SIGNATURE, "a{sv}");
    }
}
