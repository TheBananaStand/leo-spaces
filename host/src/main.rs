//! `leo-spaces`, hosted: what `leo-package@spaces.service` execs from
//! `/usr/lib/leo/packages/leo-spaces`.
//!
//! `main_for_crate!`, not `main_for`: the macro expands
//! `env!("CARGO_PKG_VERSION")` here, so the artifact — and the pacman pkgver
//! `build-package-pkg.sh` reads from it — is this package's version and not
//! leo-package-host's.

/// Where the hub answers when nothing says otherwise — `leo_core`'s default
/// listen address, and what `leo-pkg-admin provision-host` writes.
const DEFAULT_API_URL: &str = "http://127.0.0.1:8420";

fn main() -> anyhow::Result<()> {
    // **A store-installed hosted package has no `/etc/leo-<name>/env`.** Only
    // `leo-pkg-admin provision-host` writes one (LEO_API_URL, LEO_HUB_USER);
    // the store arm's `install-package` verb makes the directory and nothing
    // in it but the token, and `leo-package@.service` sets LEO_HUB_USER and
    // the token FILE but no LEO_API_URL. `leo_sdk_core::Credential::from_vars`
    // and `HostConfig::from_env` both need the URL, so without this the host
    // runs with a token and no graph: no settings mirror, so no Host, so every
    // call is the local-machine refusal — on a correctly installed package.
    //
    // Defaulted only when a token file is configured (i.e. under the unit),
    // so a scratch `--artifact` run stays hub-less. Set here, before the
    // runtime exists, while this process has exactly one thread. Remove when
    // the hub writes the env file or the host defaults it (reported to the
    // board; see the knowledge entry for this conversion).
    if std::env::var_os("LEO_API_URL").is_none_or(|v| v.is_empty())
        && std::env::var_os("LEO_PACKAGE_TOKEN_FILE").is_some()
    {
        std::env::set_var("LEO_API_URL", DEFAULT_API_URL);
    }
    leo_package_host::main_for_crate!(leo_spaces_host::SpacesPackage)
}
