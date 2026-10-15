//! Settings, as the hub hands them down.
//!
//! The hub launches this binary with each entitled setting exported as an
//! environment variable named by its settings key **verbatim and lower-case**.
//! Reading only the shouted form finds nothing: the package installs, builds,
//! launches, and complains that a setting the owner definitely filled in is
//! missing. The uppercase name is the fallback, for running this by hand.

use crate::host::Host;

#[derive(Debug, Clone)]
pub struct Config {
    /// Which machine runs the guests. Empty means this one.
    pub host: Host,
    /// The hub's own API, for resolving a caller's bearer token.
    pub leo_api_url: String,
    /// guacd, for the browser console. Optional — spaces work without it.
    pub guacd_addr: String,
    pub listen_port: u16,
}

fn setting(key: &str) -> Option<String> {
    std::env::var(key)
        .or_else(|_| std::env::var(key.to_ascii_uppercase()))
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

impl Config {
    pub fn from_env() -> Self {
        let host = Host::from_settings(
            setting("spaces_host"),
            setting("spaces_ssh_user"),
            setting("spaces_ssh_port").and_then(|v| v.parse().ok()),
            setting("spaces_ssh_key"),
        );

        Self {
            host,
            leo_api_url: setting("LEO_API_URL")
                .unwrap_or_else(|| "http://127.0.0.1:8420".to_string()),
            guacd_addr: setting("guacd_addr").unwrap_or_else(|| "127.0.0.1:4822".to_string()),
            listen_port: std::env::var("LEO_APP_PORT")
                .or_else(|_| std::env::var("PORT"))
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(8530),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The hub sends `spaces_host`, lower-case. A package reading only
    /// `SPACES_HOST` finds nothing and reports a setting the owner filled in
    /// as missing — on Linux, where env names are case-sensitive.
    #[test]
    fn a_setting_is_read_under_the_name_the_hub_sends() {
        // SAFETY: single-threaded test with keys unique to it.
        unsafe {
            std::env::set_var("spaces_probe_lower", "from-hub");
            std::env::set_var("SPACES_PROBE_UPPER", "by-hand");
            std::env::set_var("spaces_probe_blank", "   ");
        }
        assert_eq!(setting("spaces_probe_lower").as_deref(), Some("from-hub"));
        assert_eq!(setting("spaces_probe_upper").as_deref(), Some("by-hand"));
        // Whitespace is not a value. A host of "  " would become an ssh to
        // nowhere instead of meaning "this box".
        assert_eq!(setting("spaces_probe_blank"), None);
        assert_eq!(setting("spaces_probe_absent"), None);
    }
}
