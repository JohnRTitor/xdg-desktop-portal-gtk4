use {
    super::gui::{UsbDevice, UsbUi},
    crate::{
        core::{request::run_request, response::Response},
        gui::UiProxy,
    },
    std::collections::HashMap,
    zbus::{
        Dict, ObjectServer, OwnedObjectPath, OwnedVariant, SerializeDict, Type, Variant, fdo,
        interface, message::Header,
    },
};

type UsbDeviceData = (
    String,
    HashMap<String, OwnedVariant>,
    HashMap<String, OwnedVariant>,
);

/// Udev keys read from a device, in fallback order.
const VENDOR_KEYS: [&str; 3] = ["ID_VENDOR_FROM_DATABASE", "ID_VENDOR_ENC", "ID_VENDOR_ID"];
const MODEL_KEYS: [&str; 3] = ["ID_MODEL_FROM_DATABASE", "ID_MODEL_ENC", "ID_MODEL_ID"];
const SERIAL_KEY: &str = "ID_SERIAL_SHORT";

/// A read-only view of one device's udev properties.
///
/// They reach us in one of two shapes: either as the device's own `a{sv}`, or
/// nested under a `"properties"` key. This enum unifies the two so lookups share
/// one implementation and never copy.
///
/// Only the three extracted strings are kept: the dictionary is read in place
/// and never copied.
enum UdevProperties<'a> {
    Flat(&'a HashMap<String, OwnedVariant>),
    Nested(&'a Dict<'a, 'a>),
}

impl<'a> UdevProperties<'a> {
    /// Resolves whichever shape the properties arrived in.
    ///
    /// Falls back to the flat dictionary if `"properties"` is present but is not
    /// a dictionary, so an unexpected payload does not lose every property.
    fn of(props: &'a HashMap<String, OwnedVariant>) -> Self {
        match props.get("properties").map(|value| &**value) {
            Some(Variant::Dict(dict)) => Self::Nested(dict),
            _ => Self::Flat(props),
        }
    }

    fn get(&self, key: &str) -> Option<&str> {
        match self {
            Self::Flat(map) => map.get(key).and_then(|value| as_str(value)),
            Self::Nested(dict) => {
                // `Dict::get<'d, K, V>(&'d self, key: &'k K)` carries a `'d: 'k + 'v`
                // bound that ties the (short) borrow of the dictionary to its
                // (long) key/value data lifetimes, so it cannot look up a
                // transient borrowed key on data borrowed from the message.
                // Scan for the borrowed key instead, which keeps the lookup
                // allocation-free. A device's property dictionary holds a few
                // dozen entries and this runs at most seven times per device,
                // cheaper than deep-copying the dictionary to index it by hash.
                dict.iter()
                    .find(|(k, _)| as_str(k).is_some_and(|k| k == key))
                    .and_then(|(_, value)| as_str(value))
            }
        }
    }
}

/// Reads a `Variant` as a string, tolerating one level of variant wrapping.
///
/// `a{sv}` normally deserialises to values that are already unwrapped, but a
/// payload can arrive wrapped, so tolerate one level of wrapping rather than
/// silently dropping the property.
fn as_str<'a>(value: &'a Variant<'a>) -> Option<&'a str> {
    match value {
        // One level of unwrapping, not recursive: a nested `a{sv}`'s values
        // arrive wrapped in `Variant::Variant`. Pinned by
        // `nested_values_arrive_wrapped_in_a_variant`.
        Variant::Variant(inner) => <&str>::try_from(&**inner).ok(),
        other => <&str>::try_from(other).ok(),
    }
}

#[derive(SerializeDict, Type, Debug, Default)]
#[zbus(signature = "dict")]
pub struct UsbResults {
    devices: Vec<(String, HashMap<String, OwnedVariant>)>,
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

    fn extract_property(properties: &UdevProperties<'_>, keys: &[&str]) -> Option<String> {
        keys.iter()
            .find_map(|&key| properties.get(key))
            .map(Self::parse_udev_string)
    }

    async fn acquire_devices_impl(
        &self,
        app_id: String,
        parent_window: String,
        devices_in: Vec<UsbDeviceData>,
        options: HashMap<String, OwnedVariant>,
    ) -> Response<UsbResults> {
        let mut parsed_devices = Vec::with_capacity(devices_in.len());
        for (id, props, access_options) in devices_in {
            let properties = UdevProperties::of(&props);

            // Udev properties are often hex-escaped (e.g., `\x20` for spaces).
            // We search through a series of fallback keys for the vendor and model,
            // depending on what information udev could extract.
            let vendor = Self::extract_property(&properties, &VENDOR_KEYS);
            let model = Self::extract_property(&properties, &MODEL_KEYS);

            let mut serial = None;
            if let Some(s) = properties.get(SERIAL_KEY).filter(|s| !s.is_empty()) {
                serial = Some(Self::parse_udev_string(s));
            }

            parsed_devices.push(UsbDevice {
                id,
                title: model.unwrap_or_else(|| rust_i18n::t!("unknown_device").into()),
                subtitle: vendor.unwrap_or_else(|| rust_i18n::t!("unknown_vendor").into()),
                serial,
                access_options,
            });
        }

        let activation_token = options
            .get("activation_token")
            .and_then(|v| <&str>::try_from(v).ok())
            .map(String::from);
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
            Err(e) => Response::from_ui_error(e),
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
    #[expect(
        clippy::too_many_arguments,
        reason = "AcquireDevices in https://github.com/flatpak/xdg-desktop-portal/tree/main/data/org.freedesktop.impl.portal.Usb.xml fixes five wire in-args (o, s, s, a(sa{sv}a{sv}), a{sv}); the handler also takes #[zbus(header)] and #[zbus(object_server)] arguments, so this Rust function exceeds clippy's 7-argument limit."
    )]
    #[zbus(name = "AcquireDevices")]
    async fn acquire_devices(
        &self,
        #[zbus(header)] header: Header<'_>,
        handle: OwnedObjectPath,
        parent_window: String,
        app_id: String,
        devices: Vec<UsbDeviceData>,
        options: HashMap<String, OwnedVariant>,
        #[zbus(object_server)] server: &ObjectServer,
    ) -> Result<Response<UsbResults>, fdo::Error> {
        // Borrow the sender from the message; `run_request` only needs `&str`
        // and never moves it into a spawned task, so no owned String is needed.
        let sender = header
            .sender()
            .ok_or_else(|| fdo::Error::Failed("Missing sender".into()))?;
        Ok(run_request(
            server,
            self.session_manager.clone(),
            &app_id,
            sender.as_str(),
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
    use {super::*, zbus::Type};

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
        use zbus::{
            Variant,
            wire::{self, Endian, serialized::Context},
        };

        let mut props = HashMap::new();
        props.insert(
            "name".into(),
            OwnedVariant::try_from(Variant::from("Test USB")).unwrap(),
        );

        let results = UsbResults {
            devices: vec![("device1".into(), props)],
        };

        let ctxt = Context::new(Endian::Little, 0);
        let encoded = wire::to_bytes(ctxt, &results).unwrap();
        let decoded: HashMap<String, Variant> = encoded.deserialize().unwrap().0;

        let _decoded_devices_val = decoded.get("devices").unwrap();
        assert!(decoded.contains_key("devices"));
    }
}

#[cfg(test)]
mod udev_property_tests {
    use {
        super::*,
        serde::{Serialize, de::DeserializeOwned},
        std::iter,
        zbus::wire::{Endian, serialized::Context, to_bytes},
    };

    fn owned(s: &str) -> OwnedVariant {
        OwnedVariant::try_from(Variant::from(s)).expect("str to OwnedVariant is infallible")
    }

    fn flat(pairs: &[(&str, &str)]) -> HashMap<String, OwnedVariant> {
        let source: HashMap<String, OwnedVariant> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), owned(v)))
            .collect();
        wire_round_trip(&source)
    }

    /// Wraps `inner` under a `"properties"` key, matching the nested shape the
    /// frontend sends for some devices.
    ///
    /// The whole dictionary is round-tripped through the real D-Bus encoding on
    /// purpose. The shape zbus actually produces for `a{sv}` is not the
    /// obvious one -- a nested dictionary's values arrive wrapped in
    /// `Variant::Variant` -- so hand-building it would test a payload the daemon never
    /// receives, and the unwrap in `as_str` would look like dead code.
    fn nested(outer: &[(&str, &str)], inner: &[(&str, &str)]) -> HashMap<String, OwnedVariant> {
        let inner_map: HashMap<&str, OwnedVariant> =
            inner.iter().map(|(k, v)| (*k, owned(v))).collect();
        let properties =
            OwnedVariant::try_from(Variant::from(inner_map)).expect("dict to OwnedVariant");
        let source: HashMap<String, OwnedVariant> = outer
            .iter()
            .map(|(k, v)| ((*k).to_owned(), owned(v)))
            .chain(iter::once(("properties".to_owned(), properties)))
            .collect();
        wire_round_trip(&source)
    }

    fn wire_round_trip<T>(value: &T) -> T
    where
        T: Type + Serialize + DeserializeOwned,
    {
        let ctxt = Context::new(Endian::Little, 0);
        let encoded = to_bytes(ctxt, value).expect("serialise");
        encoded.deserialize().expect("deserialise").0
    }

    fn vendor(props: &HashMap<String, OwnedVariant>) -> Option<String> {
        UsbPortal::extract_property(&UdevProperties::of(props), &VENDOR_KEYS)
    }

    fn model(props: &HashMap<String, OwnedVariant>) -> Option<String> {
        UsbPortal::extract_property(&UdevProperties::of(props), &MODEL_KEYS)
    }

    fn serial(props: &HashMap<String, OwnedVariant>) -> Option<String> {
        UdevProperties::of(props)
            .get(SERIAL_KEY)
            .filter(|s| !s.is_empty())
            .map(UsbPortal::parse_udev_string)
    }

    #[test]
    fn reads_the_flat_shape() {
        let props = flat(&[("ID_VENDOR_ENC", "Acme"), ("ID_MODEL_ENC", "Widget")]);
        assert_eq!(vendor(&props).as_deref(), Some("Acme"));
        assert_eq!(model(&props).as_deref(), Some("Widget"));
    }

    #[test]
    fn reads_the_nested_shape() {
        let props = nested(
            &[("access", "rw")],
            &[("ID_VENDOR_ENC", "Acme"), ("ID_MODEL_ENC", "Widget")],
        );
        assert_eq!(vendor(&props).as_deref(), Some("Acme"));
        assert_eq!(model(&props).as_deref(), Some("Widget"));
    }

    #[test]
    fn both_shapes_agree() {
        let flat_props = flat(&[("ID_VENDOR_FROM_DATABASE", "Acme")]);
        let nested_props = nested(&[], &[("ID_VENDOR_FROM_DATABASE", "Acme")]);
        assert_eq!(vendor(&flat_props), vendor(&nested_props));
        assert_eq!(vendor(&flat_props).as_deref(), Some("Acme"));
    }

    #[test]
    fn nested_dictionary_wins_when_both_are_present() {
        let props = nested(&[("ID_VENDOR_ENC", "Outer")], &[("ID_VENDOR_ENC", "Inner")]);
        assert_eq!(vendor(&props).as_deref(), Some("Inner"));
    }

    #[test]
    fn a_non_dict_properties_key_falls_back_to_the_flat_dictionary() {
        let mut props = flat(&[("ID_VENDOR_ENC", "Acme")]);
        props.insert("properties".to_owned(), owned("not a dict"));
        assert_eq!(
            vendor(&props).as_deref(),
            Some("Acme"),
            "an unexpected payload must not lose every property",
        );
    }

    #[test]
    fn vendor_keys_fall_back_in_order() {
        let props = flat(&[
            ("ID_VENDOR_ENC", "Hex"),
            ("ID_VENDOR_FROM_DATABASE", "Readable"),
        ]);
        assert_eq!(vendor(&props).as_deref(), Some("Readable"));
        assert_eq!(
            vendor(&flat(&[("ID_VENDOR_ENC", "Hex")])).as_deref(),
            Some("Hex")
        );
        assert_eq!(
            vendor(&flat(&[("ID_VENDOR_ID", "0abc")])).as_deref(),
            Some("0abc"),
            "the raw id is the last fallback"
        );
    }

    #[test]
    fn missing_properties_yield_none() {
        assert_eq!(vendor(&flat(&[("ID_MODEL_ENC", "Widget")])), None);
        assert_eq!(model(&flat(&[("ID_VENDOR_ENC", "Acme")])), None);
        assert_eq!(serial(&flat(&[("ID_VENDOR_ENC", "Acme")])), None);
    }

    #[test]
    fn non_string_values_fall_through_to_the_next_key() {
        let mut props = flat(&[]);
        props.insert(
            "ID_VENDOR_ENC".to_owned(),
            OwnedVariant::try_from(Variant::from(42u32)).unwrap(),
        );
        props.insert("ID_VENDOR_ID".to_owned(), owned("0abc"));
        assert_eq!(vendor(&props).as_deref(), Some("0abc"));
    }

    #[test]
    fn hex_escapes_are_decoded_in_both_shapes() {
        for props in [
            flat(&[("ID_MODEL_ENC", "Big\\x20Widget")]),
            nested(&[], &[("ID_MODEL_ENC", "Big\\x20Widget")]),
        ] {
            assert_eq!(model(&props).as_deref(), Some("Big Widget"));
        }
    }

    #[test]
    fn an_empty_serial_is_reported_as_absent() {
        assert_eq!(serial(&flat(&[("ID_SERIAL_SHORT", "")])), None);
        assert_eq!(
            serial(&flat(&[("ID_SERIAL_SHORT", "SN123")])),
            Some("SN123".to_owned()),
        );
    }

    /// Reading a handful of strings from a device must not copy its properties.
    #[test]
    fn reading_nested_properties_allocates_nothing() {
        // Asserts a *hit*, so an implementation returning `None` unconditionally
        // would not pass. `Dict` is backed by a `BTreeMap`, so the filler keys
        // must be distinct or they collapse into one entry.
        let mut inner: Vec<(String, String)> =
            vec![("ID_VENDOR_ENC".to_owned(), "Acme".to_owned())];
        inner.extend((0..40).map(|i| (format!("ID_PROP_{i:02}"), "value".to_owned())));
        inner.sort();
        let inner_refs: Vec<(&str, &str)> = inner
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();

        let props = nested(&[], &inner_refs);
        let view = UdevProperties::of(&props);
        let scope = crate::alloc_probe::AllocScope::start();
        let read = view.get("ID_VENDOR_ENC");
        let snap = scope.finish();

        assert_eq!(read, Some("Acme"), "a borrowed lookup must still find it");
        assert_eq!(
            snap.count, 0,
            "a borrowed lookup must not copy the dictionary, got {snap:?}",
        );
    }

    /// Guards the shape assumption: a nested `a{sv}` really does arrive with its
    /// values wrapped in `Variant::Variant`, so the unwrap in `as_str` is
    /// load-bearing rather than defensive noise.
    #[test]
    fn nested_values_arrive_wrapped_in_a_variant() {
        let props = nested(&[], &[("ID_VENDOR_ENC", "Acme")]);
        let nested_value = props.get("properties").expect("properties key");
        let Variant::Dict(dict) = &**nested_value else {
            panic!("properties must be a dictionary");
        };
        let wrapped: Vec<bool> = dict
            .iter()
            .map(|(_, v)| matches!(v, Variant::Variant(_)))
            .collect();
        assert_eq!(
            wrapped,
            vec![true],
            "zbus wraps a{{sv}} values; if that ever changes the unwrap in \
             `as_str` becomes dead and this assertion should be revisited"
        );
    }
}
