use zbus::interface;

pub struct Session {
    pub id: String,
}

impl Session {
    pub fn new(id: String) -> Self {
        Self { id }
    }
}

#[interface(name = "org.freedesktop.impl.portal.Session")]
impl Session {
    async fn close(&self) {
        log::info!("Session {} closed", self.id);
    }
}
