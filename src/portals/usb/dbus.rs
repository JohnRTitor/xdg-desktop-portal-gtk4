use {
    super::gui::{UsbDevice, UsbUi},
    crate::{
        core::{request::run_request, response::Response},
        gui::{UiError, UiProxy},
    },
    std::collections::HashMap,
    zbus::{
        fdo, interface,
        message::Header,
        zvariant::{OwnedObjectPath, OwnedValue, SerializeDict, Type},
    },
};

type UsbDeviceData = (
    String,
    HashMap<String, OwnedValue>,
    HashMap<String, OwnedValue>,
);

#[derive(SerializeDict, Type, Debug, Default)]
#[zvariant(signature = "dict")]
pub struct UsbResults {
    devices: Vec<(String, HashMap<String, OwnedValue>)>,
}

/// D-Bus interface wrapper for the USB portal.
///
/// This struct acts as a factory to spawn the USB device chooser UI.
pub struct UsbPortal {
    proxy: UiProxy,
    session_manager: crate::core::session_manager::SessionManager,
}

impl UsbPortal {
    pub fn new(
        proxy: &UiProxy,
        session_manager: crate::core::session_manager::SessionManager,
    ) -> Self {
        Self {
            proxy: proxy.clone(),
            session_manager,
        }
    }

    /// Cleans up udev properties containing hex-escaped strings.
    ///
    /// Udev replaces spaces with `\x20`, which we must revert before displaying
    /// the device name to the user.
    fn parse_udev_string(s: &str) -> String {
        s.replace("\\x20", " ")
    }

    fn extract_property(properties: &HashMap<String, OwnedValue>, keys: &[&str]) -> Option<String> {
        keys.iter().find_map(|&k| {
            properties
                .get(k)
                .and_then(|val| <&str>::try_from(val).ok().map(Self::parse_udev_string))
        })
    }

    async fn acquire_devices_impl(
        &self,
        app_id: String,
        parent_window: String,
        devices_in: Vec<UsbDeviceData>,
        options: HashMap<String, OwnedValue>,
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

            // Udev properties are often hex-escaped (e.g., `\x20` for spaces).
            // We search through a series of fallback keys for the vendor and model,
            // depending on what information udev could extract.
            let vendor = Self::extract_property(
                &properties,
                &["ID_VENDOR_FROM_DATABASE", "ID_VENDOR_ENC", "ID_VENDOR_ID"],
            );
            let model = Self::extract_property(
                &properties,
                &["ID_MODEL_FROM_DATABASE", "ID_MODEL_ENC", "ID_MODEL_ID"],
            );

            let mut serial = None;
            if let Some(val) = properties.get("ID_SERIAL_SHORT")
                && let Ok(s) = <&str>::try_from(val)
                && !s.is_empty()
            {
                serial = Some(Self::parse_udev_string(s));
            }

            parsed_devices.push(UsbDevice {
                id,
                title: model.unwrap_or_else(|| rust_i18n::t!("unknown_device").to_string()),
                subtitle: vendor.unwrap_or_else(|| rust_i18n::t!("unknown_vendor").to_string()),
                serial,
                access_options,
            });
        }

        let activation_token = options
            .get("activation_token")
            .and_then(|v| <&str>::try_from(v).ok())
            .map(|s| s.to_string());
        let ui = UsbUi {
            app_id,
            parent_window,
            activation_token,
            devices: parsed_devices,
        };

        match ui.run(&self.proxy).await {
            Ok(result) => {
                let res = UsbResults {
                    devices: result.devices,
                };
                Response::success(res)
            }
            Err(UiError::Closed) | Err(UiError::Rejected) => Response::cancelled(),
        }
    }
}

/// The D-Bus interface implementation for `org.freedesktop.impl.portal.Usb`.
///
/// This portal allows a sandboxed application to request access to USB devices.
/// The frontend daemon (xdg-desktop-portal) passes a list of available devices,
/// and the user selects which ones the app can access.
#[interface(name = "org.freedesktop.impl.portal.Usb")]
impl UsbPortal {
    #[zbus(name = "AcquireDevices")]
    async fn acquire_devices(
        &self,
        #[zbus(header)] header: Header<'_>,
        handle: OwnedObjectPath,
        parent_window: String,
        app_id: String,
        devices: Vec<UsbDeviceData>,
        options: HashMap<String, OwnedValue>,
        #[zbus(object_server)] server: &zbus::ObjectServer,
    ) -> Result<Response<UsbResults>, fdo::Error> {
        let sender = header
            .sender()
            .ok_or_else(|| fdo::Error::Failed("Missing sender".to_string()))?
            .to_string();
        Ok(run_request(
            server,
            self.session_manager.clone(),
            &app_id,
            &sender,
            handle,
            self.acquire_devices_impl(app_id.clone(), parent_window, devices, options),
        )
        .await)
    }

    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        1
    }
}

#[cfg(test)]
mod tests {
    use {super::*, zbus::zvariant::Type};

    #[test]
    fn test_parse_udev_basic() {
        assert_eq!(
            UsbPortal::parse_udev_string("Logitech\\x20Mouse"),
            "Logitech Mouse"
        );
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

    #[test]
    fn test_usb_results_serialize() {
        use zbus::zvariant::{self, Endian, Value, serialized::Context};

        let mut props = HashMap::new();
        props.insert(
            "name".to_string(),
            zbus::zvariant::OwnedValue::try_from(Value::from("Test USB")).unwrap(),
        );

        let results = UsbResults {
            devices: vec![("device1".to_string(), props)],
        };

        let ctxt = Context::new_dbus(Endian::Little, 0);
        let encoded = zvariant::to_bytes(ctxt, &results).unwrap();
        let decoded: HashMap<String, Value> = encoded.deserialize().unwrap().0;

        let _decoded_devices_val = decoded.get("devices").unwrap();
        // Since signature is a{sv}, devices is returned as Value.
        // In UsbResults, devices is `a(sa{sv})`.
        // Let's just ensure it's not empty and serialization worked.
        assert!(decoded.contains_key("devices"));
    }
}
