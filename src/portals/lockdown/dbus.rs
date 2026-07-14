use zbus::interface;

pub struct LockdownPortal {}

impl LockdownPortal {
    pub fn new() -> Self {
        Self {}
    }
}

#[interface(name = "org.freedesktop.impl.portal.Lockdown")]
impl LockdownPortal {
    #[zbus(property, name = "disable-printing")]
    async fn disable_printing(&self) -> bool {
        false
    }

    #[zbus(property, name = "disable-printing")]
    async fn set_disable_printing(&self, _value: bool) -> zbus::fdo::Result<()> {
        Err(zbus::fdo::Error::NotSupported("Lockdown portal is read-only".into()))
    }

    #[zbus(property, name = "disable-save-to-disk")]
    async fn disable_save_to_disk(&self) -> bool {
        false
    }

    #[zbus(property, name = "disable-save-to-disk")]
    async fn set_disable_save_to_disk(&self, _value: bool) -> zbus::fdo::Result<()> {
        Err(zbus::fdo::Error::NotSupported("Lockdown portal is read-only".into()))
    }

    #[zbus(property, name = "disable-application-handlers")]
    async fn disable_application_handlers(&self) -> bool {
        false
    }

    #[zbus(property, name = "disable-application-handlers")]
    async fn set_disable_application_handlers(&self, _value: bool) -> zbus::fdo::Result<()> {
        Err(zbus::fdo::Error::NotSupported("Lockdown portal is read-only".into()))
    }

    #[zbus(property, name = "disable-location")]
    async fn disable_location(&self) -> bool {
        false
    }

    #[zbus(property, name = "disable-location")]
    async fn set_disable_location(&self, _value: bool) -> zbus::fdo::Result<()> {
        Err(zbus::fdo::Error::NotSupported("Lockdown portal is read-only".into()))
    }

    #[zbus(property, name = "disable-camera")]
    async fn disable_camera(&self) -> bool {
        false
    }

    #[zbus(property, name = "disable-camera")]
    async fn set_disable_camera(&self, _value: bool) -> zbus::fdo::Result<()> {
        Err(zbus::fdo::Error::NotSupported("Lockdown portal is read-only".into()))
    }

    #[zbus(property, name = "disable-microphone")]
    async fn disable_microphone(&self) -> bool {
        false
    }

    #[zbus(property, name = "disable-microphone")]
    async fn set_disable_microphone(&self, _value: bool) -> zbus::fdo::Result<()> {
        Err(zbus::fdo::Error::NotSupported("Lockdown portal is read-only".into()))
    }

    #[zbus(property, name = "disable-sound-output")]
    async fn disable_sound_output(&self) -> bool {
        false
    }

    #[zbus(property, name = "disable-sound-output")]
    async fn set_disable_sound_output(&self, _value: bool) -> zbus::fdo::Result<()> {
        Err(zbus::fdo::Error::NotSupported("Lockdown portal is read-only".into()))
    }
}
