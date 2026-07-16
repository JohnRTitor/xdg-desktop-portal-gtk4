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
            // Parse XID as a hex string (e.g. "x11:3f0000a" or "x11:0x800004")
            let hex_str = x11_handle_str.strip_prefix("0x").unwrap_or(x11_handle_str);
            if let Ok(xid) = u64::from_str_radix(hex_str, 16) {
                return Some(Self::X11(xid));
            } else {
                log::warn!("Invalid X11 window handle: {}", handle);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_wayland() {
        assert_eq!(
            WindowIdentifier::parse("wayland:test"),
            Some(WindowIdentifier::Wayland("test".to_string()))
        );
        assert_eq!(WindowIdentifier::parse("wayland:"), None);
    }

    #[test]
    fn test_parse_x11() {
        assert_eq!(
            WindowIdentifier::parse("x11:3f0000a"),
            Some(WindowIdentifier::X11(0x3f0000a))
        );
        assert_eq!(
            WindowIdentifier::parse("x11:0x800004"),
            Some(WindowIdentifier::X11(0x800004))
        );
        assert_eq!(WindowIdentifier::parse("x11:invalid"), None);
    }

    #[test]
    fn test_parse_invalid() {
        assert_eq!(WindowIdentifier::parse("invalid"), None);
        assert_eq!(WindowIdentifier::parse(""), None);
    }
}
