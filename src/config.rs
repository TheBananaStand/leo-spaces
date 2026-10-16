//! Settings — as the hub hands them down, and, when it does not, as this
//! process asks for them back.
//!
//! There are two ways in, because the hub has two.
//!
//! **The environment.** The hub launches a *dynamic*-port app package with
//! each entitled setting exported as an environment variable named by its
//! settings key **verbatim and lower-case**. Reading only the shouted form
//! finds nothing: the package installs, builds, launches, and complains that a
//! setting the owner definitely filled in is missing. The uppercase name is
//! the fallback, for running this by hand. The MCP subprocess is launched this
//! way too, so in `leo-spaces mcp` the environment is normally the whole
//! story.
//!
//! **`GET /graph/v1/settings`.** That hand-off is gated on `app.port == 0` in
//! the hub's `spawn_app`: a dynamic-port app is handed its port, data dir, API
//! URL and entitled settings; a **fixed**-port one is handed *nothing*. This
//! package declares a fixed port deliberately — that is what keeps its HTTP
//! half out of the bubblewrap jail, which has no `/run/libvirt` in it and so
//! can never reach libvirtd (see the README). The price is this module: the
//! four `spaces_*` settings are read back from the hub's Graph API with a
//! package token from `/etc/leo-spaces/token`, which
//! `scripts/install-admin-wrapper.sh` places there root-owned and readable
//! only by the account the hub runs as.
//!
//! The environment wins wherever both have a value, so nothing about running
//! this by hand changes, and a hub that is slow or down costs a few seconds at
//! startup rather than a failure to start.

use std::collections::BTreeMap;
use std::time::Duration;

use crate::host::Host;

/// The settings keys this package may read — the same four the descriptor
/// declares in `entitlements.settings_read`, which is what the hub checks the
/// Graph reply against. Named here so the request and the merge agree.
pub const SETTING_KEYS: [&str; 4] = [
    "spaces_host",
    "spaces_ssh_user",
    "spaces_ssh_port",
    "spaces_ssh_key",
];

/// How long the hub gets to answer before this process gives up and starts on
/// what the environment carried.
///
/// Short on purpose. The hub spawns this process and then watches its port for
/// five seconds to decide whether it came up, so time spent waiting on the
/// hub's own API is time the hub spends deciding this app is unhealthy.
const HUB_TIMEOUT: Duration = Duration::from_secs(3);

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

/// The four settings that decide what [`Host`] this process talks to, before
/// they are resolved into one.
///
/// Kept as text and merged as text: whichever source supplies `spaces_host`
/// should supply the ssh fields with it, and parsing in the middle would make
/// "absent" and "unparseable" the same thing a merge later cannot tell apart.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct HostSettings {
    host: Option<String>,
    ssh_user: Option<String>,
    ssh_port: Option<String>,
    ssh_key: Option<String>,
}

fn setting(key: &str) -> Option<String> {
    std::env::var(key)
        .or_else(|_| std::env::var(key.to_ascii_uppercase()))
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

impl HostSettings {
    fn from_env() -> Self {
        Self {
            host: setting(SETTING_KEYS[0]),
            ssh_user: setting(SETTING_KEYS[1]),
            ssh_port: setting(SETTING_KEYS[2]),
            ssh_key: setting(SETTING_KEYS[3]),
        }
    }

    fn from_map(m: &BTreeMap<String, String>) -> Self {
        let get = |k: &str| {
            m.get(k)
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        Self {
            host: get(SETTING_KEYS[0]),
            ssh_user: get(SETTING_KEYS[1]),
            ssh_port: get(SETTING_KEYS[2]),
            ssh_key: get(SETTING_KEYS[3]),
        }
    }

    /// Take `other`'s values for the fields this one has none of.
    ///
    /// A fallback, never an override: the environment is the operator in front
    /// of the machine, and the hub's stored settings are what the install was
    /// configured with.
    fn fill_from(&mut self, other: Self) {
        self.host = self.host.take().or(other.host);
        self.ssh_user = self.ssh_user.take().or(other.ssh_user);
        self.ssh_port = self.ssh_port.take().or(other.ssh_port);
        self.ssh_key = self.ssh_key.take().or(other.ssh_key);
    }

    fn into_host(self) -> Host {
        Host::from_settings(
            self.host,
            self.ssh_user,
            self.ssh_port.and_then(|v| v.parse().ok()),
            self.ssh_key,
        )
    }
}

impl Config {
    /// Environment only. Synchronous, so tests and `--help`-shaped paths can
    /// have a config without a runtime or a hub.
    pub fn from_env() -> Self {
        Self::with_settings(HostSettings::from_env())
    }

    /// The real entry point: the environment, then the hub for whatever the
    /// environment did not carry.
    pub async fn load() -> Self {
        let mut settings = HostSettings::from_env();
        let base = Self::with_settings(settings.clone());

        let Some(token) = package_token() else {
            // No token is what a by-hand run looks like, and what an install
            // that only ever virtualises on this box looks like. Both are
            // legitimate: all four settings describe a *remote* host, and the
            // common case has none of them.
            tracing::debug!("no package token — settings come from the environment alone");
            return base;
        };

        match tokio::time::timeout(
            HUB_TIMEOUT,
            fetch_settings(&base.leo_api_url, &token),
        )
        .await
        {
            Ok(Ok(from_hub)) => {
                settings.fill_from(HostSettings::from_map(&from_hub));
                Self::with_settings(settings)
            }
            // Degraded, not fatal, and said out loud either way. The hub's own
            // advice for this endpoint is that a package should refuse to start
            // on a missing key rather than come up unconfigured — right for a
            // package whose credentials are mandatory, wrong for this one,
            // where every one of the four is optional and "none of them" is the
            // common case meaning "virtualise on this box".
            Ok(Err(e)) => {
                tracing::warn!("could not read settings from the hub ({e}) — using the environment");
                base
            }
            Err(_) => {
                tracing::warn!(
                    "the hub did not answer /graph/v1/settings within {}s — using the environment",
                    HUB_TIMEOUT.as_secs()
                );
                base
            }
        }
    }

    fn with_settings(settings: HostSettings) -> Self {
        Self {
            host: settings.into_host(),
            leo_api_url: setting("LEO_API_URL")
                .unwrap_or_else(|| "http://127.0.0.1:8420".to_string()),
            guacd_addr: setting("guacd_addr").unwrap_or_else(|| "127.0.0.1:4822".to_string()),
            listen_port: std::env::var("LEO_APP_PORT")
                .or_else(|_| std::env::var("PORT"))
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(DEFAULT_PORT),
        }
    }
}

/// The port this binary serves on when nothing in the environment says
/// otherwise — which, as a fixed-port app package, is always: the hub exports
/// `LEO_APP_PORT` only for the dynamic-port case.
///
/// It must equal `app.port` in `descriptor.json`, because that is the port the
/// hub proxies `/p/spaces/*` to and the one it probes to decide whether this
/// unit is already serving. `the_declared_port_is_the_one_this_binary_binds`
/// is the test that says so.
pub const DEFAULT_PORT: u16 = 8530;

/// Where a package token may be left for this process, most explicit first.
///
/// A file rather than an environment variable because the hub spawns a
/// fixed-port app with *its own* environment and nothing added — there is no
/// hand-off to put a secret in. `LEO_PKG_TOKEN_FILE` is the name the hub's own
/// `leo packages service` units use, honoured here so a hand-written unit
/// looks like the generated one; [`TOKEN_PATH`] is the convention for this
/// package.
pub const TOKEN_PATH: &str = "/etc/leo-spaces/token";

fn package_token() -> Option<String> {
    if let Some(t) = setting("LEO_PKG_TOKEN") {
        return Some(t);
    }
    let named = setting("LEO_PKG_TOKEN_FILE");
    let path = named.clone().unwrap_or_else(|| TOKEN_PATH.to_string());
    match std::fs::read_to_string(&path) {
        Ok(t) if !t.trim().is_empty() => Some(t.trim().to_string()),
        // An environment variable that *names* a token file is a statement that
        // one exists, so an empty or unreadable file there is a
        // misconfiguration worth naming. The default path is a convention, and
        // its absence is the ordinary local-only install.
        Ok(_) if named.is_some() => {
            tracing::warn!("{path} is empty — no settings will be read from the hub");
            None
        }
        Err(e) if named.is_some() => {
            tracing::warn!("cannot read {path}: {e} — no settings will be read from the hub");
            None
        }
        _ => None,
    }
}

/// What `GET /graph/v1/settings` answers. `missing` is every entitled key with
/// no value — reported rather than omitted, so a reader can tell "empty" from
/// "I could not read it".
#[derive(Debug, serde::Deserialize)]
struct SettingsReply {
    #[serde(default)]
    package: String,
    #[serde(default)]
    settings: BTreeMap<String, String>,
    #[serde(default)]
    missing: Vec<String>,
}

/// Ask the hub for this package's own settings.
///
/// `Connection: close` and read to EOF: the hub closes the socket when the
/// body is done, so there is no keep-alive bookkeeping and no second request
/// this could be confused with. Both framings the server may choose are
/// handled — a `Content-Length` body and a chunked one — because which one
/// arrives is hyper's decision, not this caller's, and getting it wrong looks
/// like malformed JSON rather than like a framing bug.
async fn fetch_settings(api_url: &str, token: &str) -> anyhow::Result<BTreeMap<String, String>> {
    use anyhow::Context as _;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    /// Enough for every entitled setting several times over, and a bound on
    /// what a wrong endpoint can make this process allocate.
    const MAX_REPLY: u64 = 64 * 1024;

    // Everything that goes into a request line or a header is checked first.
    // Both of these arrive from outside this process — `LEO_API_URL` from the
    // environment, the token from a file — and a `\r\n` in either would end
    // the line early and let the rest be read as a header of its own. The
    // values are a loopback authority and an opaque credential; neither has
    // any business containing a control character, so this costs nothing and
    // removes the question.
    let authority = api_url
        .trim_end_matches('/')
        .strip_prefix("http://")
        .with_context(|| format!("LEO_API_URL must be a plain http:// loopback URL, got {api_url}"))?
        // `http://127.0.0.1:8420/api` is a reasonable thing to be handed, and
        // `TcpStream::connect` cannot parse a path. Take the authority.
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default();
    header_safe(authority, "LEO_API_URL")?;
    header_safe(token, "the package token")?;

    let mut stream = tokio::net::TcpStream::connect(authority)
        .await
        .with_context(|| format!("connecting to the hub at {authority}"))?;

    stream
        .write_all(
            format!(
                "GET /graph/v1/settings HTTP/1.1\r\n\
                 Host: {authority}\r\n\
                 Authorization: Bearer {token}\r\n\
                 Accept: application/json\r\n\
                 Connection: close\r\n\r\n"
            )
            .as_bytes(),
        )
        .await?;

    let mut buf = Vec::new();
    (&mut stream).take(MAX_REPLY).read_to_end(&mut buf).await?;

    let mut headers = [httparse::EMPTY_HEADER; 32];
    let mut res = httparse::Response::new(&mut headers);
    let body_at = match res.parse(&buf)? {
        httparse::Status::Complete(n) => n,
        httparse::Status::Partial => anyhow::bail!("the hub's reply ended mid-header"),
    };
    let status = res.code.unwrap_or(0);
    let chunked = res.headers.iter().any(|h| {
        h.name.eq_ignore_ascii_case("transfer-encoding")
            && String::from_utf8_lossy(h.value)
                .to_ascii_lowercase()
                .contains("chunked")
    });
    let body = if chunked {
        dechunk(&buf[body_at..])?
    } else {
        buf[body_at..].to_vec()
    };

    if !(200..300).contains(&status) {
        // The body carries the reason — a revoked token, a scope the mint did
        // not include — and without it the operator gets a bare 403.
        anyhow::bail!(
            "the hub answered {status}: {}",
            String::from_utf8_lossy(&body).trim()
        );
    }

    let reply: SettingsReply = serde_json::from_slice(&body)
        .context("the hub's settings reply was not the JSON this expects")?;

    // Echoed by the hub, so a token belonging to another package is found out
    // here rather than as three configuration mysteries later.
    if !reply.package.is_empty() && reply.package != PACKAGE {
        anyhow::bail!(
            "this token belongs to package '{}', not '{PACKAGE}'",
            reply.package
        );
    }
    tracing::info!(
        "read {} setting(s) from the hub{}",
        reply.settings.len(),
        if reply.missing.is_empty() {
            String::new()
        } else {
            format!(" (not set: {})", reply.missing.join(", "))
        }
    );
    Ok(reply.settings)
}

/// This package's name, as the hub knows it — the `name` in `descriptor.json`,
/// which is what a package token is bound to.
const PACKAGE: &str = "spaces";

/// Refuse a value that cannot appear in a header line.
///
/// Space is included deliberately: neither an authority nor a bearer token may
/// contain one, and a token file that picked up a stray word is a
/// misconfiguration worth refusing by name rather than sending.
fn header_safe(value: &str, what: &str) -> anyhow::Result<()> {
    if value.is_empty() {
        anyhow::bail!("{what} is empty");
    }
    if let Some(bad) = value
        .chars()
        .find(|c| c.is_control() || c.is_whitespace() || !c.is_ascii())
    {
        anyhow::bail!("{what} contains {bad:?}, which cannot go in an HTTP header");
    }
    Ok(())
}

/// Reassemble a `Transfer-Encoding: chunked` body.
///
/// Small on purpose: it reads the size lines and the data between them and
/// stops at the terminating zero. Trailers after it are ignored, chunk
/// extensions (`1a;name=value`) are cut at the semicolon, and anything that
/// does not parse is an error rather than a partial body — a truncated reply
/// silently treated as whole is how a package comes up with half its settings.
fn dechunk(mut body: &[u8]) -> anyhow::Result<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let nl = body
            .windows(2)
            .position(|w| w == b"\r\n")
            .ok_or_else(|| anyhow::anyhow!("chunked body ended without a size line"))?;
        let line = String::from_utf8_lossy(&body[..nl]);
        let size = usize::from_str_radix(line.split(';').next().unwrap_or("").trim(), 16)
            .map_err(|e| anyhow::anyhow!("bad chunk size '{}': {e}", line.trim()))?;
        body = &body[nl + 2..];
        if size == 0 {
            return Ok(out);
        }
        if body.len() < size {
            anyhow::bail!("chunked body is shorter than its size line claims");
        }
        out.extend_from_slice(&body[..size]);
        // Past the chunk and its trailing CRLF.
        body = body.get(size + 2..).unwrap_or(&[]);
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

    /// The port in `descriptor.json` is the port this binary binds.
    ///
    /// This is the pair the fixed-port decision rests on and nothing at
    /// runtime would complain about: the hub proxies `/p/spaces/*` to
    /// `app.port` and probes it to decide whether the service is already
    /// serving, while the binary — handed no `LEO_APP_PORT`, because that
    /// hand-off is for dynamic-port packages — binds its own default. Set them
    /// differently and the install succeeds, the unit starts, the page 502s,
    /// and the hub launches a *second* copy because the port it watches is
    /// silent.
    #[test]
    fn the_declared_port_is_the_one_this_binary_binds() {
        let descriptor: serde_json::Value =
            serde_json::from_str(include_str!("../descriptor.json")).unwrap();
        let declared = descriptor["app"]["port"].as_u64().unwrap();
        assert_eq!(
            declared, DEFAULT_PORT as u64,
            "descriptor.json app.port is {declared} but this binary binds {DEFAULT_PORT}"
        );
        // And it is not zero. Zero is the dynamic-port case, which is the
        // bubblewrap-jailed one: no /run/libvirt, so no hypervisor.
        assert_ne!(declared, 0);
    }

    /// Every key this merges is a key the descriptor entitles it to read.
    ///
    /// The Graph endpoint answers from `entitlements.settings_read`, so a key
    /// named here and not there is simply never returned — and the symptom is
    /// a setting the owner filled in having no effect, with nothing logged.
    #[test]
    fn the_settings_it_reads_are_the_settings_it_declared() {
        let descriptor: serde_json::Value =
            serde_json::from_str(include_str!("../descriptor.json")).unwrap();
        let declared: Vec<&str> = descriptor["entitlements"]["settings_read"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        for key in SETTING_KEYS {
            assert!(
                declared.contains(&key),
                "{key} is read by config.rs but not declared in entitlements.settings_read"
            );
        }
    }

    /// The environment wins; the hub fills the gaps.
    #[test]
    fn the_environment_wins_and_the_hub_fills_the_rest() {
        let mut env = HostSettings {
            host: Some("in-the-unit".into()),
            ..Default::default()
        };
        env.fill_from(HostSettings {
            host: Some("in-the-hub".into()),
            ssh_user: Some("leo".into()),
            ssh_port: Some("2222".into()),
            ssh_key: None,
        });
        assert_eq!(env.host.as_deref(), Some("in-the-unit"));
        assert_eq!(env.ssh_user.as_deref(), Some("leo"));
        assert_eq!(env.ssh_port.as_deref(), Some("2222"));
        assert_eq!(env.ssh_key, None);
    }

    /// Nothing that cannot sit on a header line is sent on one.
    ///
    /// The token is read from a file and the authority from the environment;
    /// a CRLF in either would end the request line early and have the rest
    /// read as headers. Checked rather than escaped, because there is no
    /// legitimate value here that needs escaping.
    #[test]
    fn a_value_with_a_newline_in_it_is_refused() {
        assert!(header_safe("127.0.0.1:8420", "authority").is_ok());
        assert!(header_safe("leo_pkg_abc123", "token").is_ok());

        for bad in [
            "leo_pkg_a\r\nX-Injected: 1",
            "leo_pkg_a\nX-Injected: 1",
            "127.0.0.1:8420\r\nHost: elsewhere",
            "leo_pkg a",
            "leo_pkg_\u{7f}",
            "",
        ] {
            assert!(
                header_safe(bad, "value").is_err(),
                "{bad:?} should be refused"
            );
        }
    }

    /// A blank value from the hub is not a value.
    ///
    /// `settings` only carries keys that have one — but a key set to spaces is
    /// a value as far as the database is concerned, and `"  "` as a host is an
    /// ssh to nowhere rather than "this box".
    #[test]
    fn a_blank_setting_from_the_hub_is_not_a_value() {
        let map = BTreeMap::from([
            ("spaces_host".to_string(), "   ".to_string()),
            ("spaces_ssh_user".to_string(), " leo ".to_string()),
        ]);
        let s = HostSettings::from_map(&map);
        assert_eq!(s.host, None);
        assert_eq!(s.ssh_user.as_deref(), Some("leo"));
    }

    /// An unparseable ssh port does not silently become a *different* host.
    ///
    /// `Host::from_settings` decides what a remote is from the host field; the
    /// port falling back to the default is right, and the host disappearing
    /// because the port was junk would not be.
    #[test]
    fn a_junk_ssh_port_does_not_lose_the_host() {
        let s = HostSettings {
            host: Some("compute-1".into()),
            ssh_port: Some("not-a-number".into()),
            ..Default::default()
        };
        assert!(matches!(s.into_host(), Host::Remote { .. }));
    }
}
