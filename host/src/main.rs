//! `leo-spaces`, hosted: what `leo-package@spaces.service` execs from
//! `/usr/lib/leo/packages/leo-spaces`.
//!
//! `main_for_crate!`, not `main_for`: the macro expands
//! `env!("CARGO_PKG_VERSION")` here, so the artifact — and the pacman pkgver
//! `build-package-pkg.sh` reads from it — is this package's version and not
//! leo-package-host's.
fn main() -> anyhow::Result<()> {
    leo_package_host::main_for_crate!(leo_spaces_host::SpacesPackage)
}
