use zbus::interface;

/// D-Bus interface wrapper for the Lockdown portal.
///
/// This struct holds no state because all lockdown policies are exposed as static
/// D-Bus properties that evaluate to `false` (meaning no restrictions).
pub struct LockdownPortal {}

impl Default for LockdownPortal {
    fn default() -> Self {
        Self::new()
    }
}

impl LockdownPortal {
    pub fn new() -> Self {
        Self {}
    }
}

/// The D-Bus interface implementation for `org.freedesktop.impl.portal.Lockdown`.
///
/// This portal allows system administrators or desktop environments to restrict
/// certain features (like printing, saving files, or using devices) for sandboxed apps.
///
/// Currently, this implementation defaults to allowing everything (returning `false` for all
/// `disable-*` properties). A more complete implementation might read these settings from
/// GSettings or a configuration file.
#[interface(name = "org.freedesktop.impl.portal.Lockdown")]
impl LockdownPortal {
    #[zbus(property, name = "disable-printing")]
    async fn disable_printing(&self) -> bool {
        false
    }

    // The properties are read-only for sandboxed apps, so setters always return NotSupported.
    #[zbus(property, name = "disable-printing")]
    async fn set_disable_printing(&self, _value: bool) -> zbus::fdo::Result<()> {
        Err(zbus::fdo::Error::NotSupported(
            "Lockdown portal is read-only".into(),
        ))
    }

    #[zbus(property, name = "disable-save-to-disk")]
    async fn disable_save_to_disk(&self) -> bool {
        false
    }

    #[zbus(property, name = "disable-save-to-disk")]
    async fn set_disable_save_to_disk(&self, _value: bool) -> zbus::fdo::Result<()> {
        Err(zbus::fdo::Error::NotSupported(
            "Lockdown portal is read-only".into(),
        ))
    }

    #[zbus(property, name = "disable-application-handlers")]
    async fn disable_application_handlers(&self) -> bool {
        false
    }

    #[zbus(property, name = "disable-application-handlers")]
    async fn set_disable_application_handlers(&self, _value: bool) -> zbus::fdo::Result<()> {
        Err(zbus::fdo::Error::NotSupported(
            "Lockdown portal is read-only".into(),
        ))
    }

    #[zbus(property, name = "disable-location")]
    async fn disable_location(&self) -> bool {
        false
    }

    #[zbus(property, name = "disable-location")]
    async fn set_disable_location(&self, _value: bool) -> zbus::fdo::Result<()> {
        Err(zbus::fdo::Error::NotSupported(
            "Lockdown portal is read-only".into(),
        ))
    }

    #[zbus(property, name = "disable-camera")]
    async fn disable_camera(&self) -> bool {
        false
    }

    #[zbus(property, name = "disable-camera")]
    async fn set_disable_camera(&self, _value: bool) -> zbus::fdo::Result<()> {
        Err(zbus::fdo::Error::NotSupported(
            "Lockdown portal is read-only".into(),
        ))
    }

    #[zbus(property, name = "disable-microphone")]
    async fn disable_microphone(&self) -> bool {
        false
    }

    #[zbus(property, name = "disable-microphone")]
    async fn set_disable_microphone(&self, _value: bool) -> zbus::fdo::Result<()> {
        Err(zbus::fdo::Error::NotSupported(
            "Lockdown portal is read-only".into(),
        ))
    }

    #[zbus(property, name = "disable-sound-output")]
    async fn disable_sound_output(&self) -> bool {
        false
    }

    #[zbus(property, name = "disable-sound-output")]
    async fn set_disable_sound_output(&self, _value: bool) -> zbus::fdo::Result<()> {
        Err(zbus::fdo::Error::NotSupported(
            "Lockdown portal is read-only".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_lockdown_properties() {
        let portal = LockdownPortal::new();

        assert!(!portal.disable_printing().await);
        assert!(matches!(
            portal.set_disable_printing(true).await,
            Err(zbus::fdo::Error::NotSupported(_))
        ));

        assert!(!portal.disable_save_to_disk().await);
        assert!(matches!(
            portal.set_disable_save_to_disk(true).await,
            Err(zbus::fdo::Error::NotSupported(_))
        ));

        assert!(!portal.disable_application_handlers().await);
        assert!(matches!(
            portal.set_disable_application_handlers(true).await,
            Err(zbus::fdo::Error::NotSupported(_))
        ));

        assert!(!portal.disable_location().await);
        assert!(matches!(
            portal.set_disable_location(true).await,
            Err(zbus::fdo::Error::NotSupported(_))
        ));

        assert!(!portal.disable_camera().await);
        assert!(matches!(
            portal.set_disable_camera(true).await,
            Err(zbus::fdo::Error::NotSupported(_))
        ));

        assert!(!portal.disable_microphone().await);
        assert!(matches!(
            portal.set_disable_microphone(true).await,
            Err(zbus::fdo::Error::NotSupported(_))
        ));

        assert!(!portal.disable_sound_output().await);
        assert!(matches!(
            portal.set_disable_sound_output(true).await,
            Err(zbus::fdo::Error::NotSupported(_))
        ));
    }
}
