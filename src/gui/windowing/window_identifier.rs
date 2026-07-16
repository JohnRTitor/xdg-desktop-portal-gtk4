#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WindowIdentifier {
    Wayland(String),
    X11(u64),
}

impl WindowIdentifier {
    /// Parses a window handle string provided by xdg-desktop-portal.
    pub fn parse(handle: &str) -> Option<Self> {
        if let Some(wayland_handle) = handle.strip_prefix("wayland:") {
            if !wayland_handle.is_empty() {
                return Some(Self::Wayland(wayland_handle.to_string()));
            }
        } else if let Some(x11_handle_str) = handle.strip_prefix("x11:") {
            // Parse XID as a hex string (e.g. "x11:3f0000a")
            if let Ok(xid) = u64::from_str_radix(x11_handle_str, 16) {
                return Some(Self::X11(xid));
            } else {
                log::warn!("Invalid X11 window handle: {}", handle);
            }
        }
        None
    }
}
