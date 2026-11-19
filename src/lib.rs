//! leo-spaces as a library — the whole package, shared by its two shapes.
//!
//! **App shape** (`src/main.rs`, the `leo-spaces` binary of this crate): the
//! hub clones this repo, builds it, and proxies `/p/spaces/*` to port 8530; the
//! tools are a hand-added stdio MCP server (`leo-spaces mcp`).
//!
//! **Hosted shape** (`host/`, its own crate and its own lockfile, so this one
//! still builds on a box with no Leo checkout): `leo-package-host` wraps a
//! `LeoPackage` built from these modules, runs it as `leo-pkg-spaces` under
//! `leo-package@spaces`, and serves it on the two sockets systemd hands it.
//!
//! Everything below is shape-agnostic except [`hosted`], which is how the host
//! crate tells the host-driving code it may not touch this machine.

use std::sync::Arc;

use axum::Router;

pub mod api;
pub mod auth;
pub mod catalog;
pub mod config;
pub mod console;
pub mod error;
pub mod facts;
pub mod guest_tools;
pub mod host;
pub mod installers;
pub mod mcp;
pub mod provision;
pub mod spaces;
pub mod ui;

/// Shared state. Deliberately small: the host is the source of truth about
/// guests, and anything cached here is a second answer that can disagree with
/// `virsh`.
#[derive(Clone)]
pub struct AppState {
    pub cfg: Arc<config::Config>,
}

/// The whole app-shape HTTP surface, assembled.
///
/// Its own function because axum decides a good deal of routing legality at
/// *runtime* — a root `nest` panics on construction and compiles perfectly on
/// the way there — so "does this router assemble" is a real question that only
/// running the code answers.
pub fn build_router(state: AppState) -> Router {
    // `merge`, not `nest("/", …)`: axum 0.8 panics on a root nest, which is a
    // runtime failure from code that type-checks.
    Router::new()
        .merge(api::router())
        .route(
            "/leo/ui/descriptor",
            axum::routing::get(ui::descriptor_handler),
        )
        .route("/leo/ui/data", axum::routing::get(ui::data_handler))
        // The guest's screen, in a window — see `console`. Also served at
        // `/api/console/{name}` (api::routes), which is the link the UI uses
        // now; this one stays for anything that still holds the old link.
        .route("/console", axum::routing::get(console::index))
        .route("/console/{name}", axum::routing::get(console::page))
        .nest_service(
            "/novnc",
            tower_http::services::ServeDir::new(console::novnc_dir()),
        )
        .with_state(state)
}

/// The router a hosted copy hands `leo-package-host` as `PackageState::router`.
///
/// The host nests it at `/api` and serves `/leo/ui/*` itself (from the
/// package's `ui_descriptor` / `ui_data`), so this is [`api::routes`] and
/// nothing else — no `/leo/ui/*`, no `/novnc` (the console page inlines its
/// bundle, so nothing fetches `/novnc` any more).
pub fn hosted_router(state: AppState) -> Router {
    api::routes().with_state(state)
}

/// Whether this process is the hosted shape.
///
/// Set once, by the host crate's `reload`, before any host command can run.
/// Read by [`host::Host`]: hosted, the process is `leo-pkg-spaces` under
/// `NoNewPrivileges=yes` with no libvirt group and no sudo, so driving *this*
/// machine as a hypervisor cannot work — and it is refused with a sentence
/// naming the two remedies rather than left to fail as an opaque `virsh` or
/// `sudo` error deep in a probe.
pub mod hosted {
    use std::sync::atomic::{AtomicBool, Ordering};

    static HOSTED: AtomicBool = AtomicBool::new(false);

    pub fn mark() {
        HOSTED.store(true, Ordering::SeqCst);
    }

    pub fn is() -> bool {
        HOSTED.load(Ordering::SeqCst)
    }

    /// What a hosted copy says when asked to drive the machine it runs on.
    pub const LOCAL_REFUSED: &str = "Spaces is running as a hosted package — its own \
        locked-down service account (leo-pkg-spaces), with no sudo and no libvirt access \
        — so it cannot drive a hypervisor on this machine. Either set Host to the machine \
        that runs the guests (with an SSH user and the package's own key, \
        /etc/leo-spaces/ssh_key) to drive it remotely, or install Spaces in app shape on \
        the hypervisor box itself.";
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> AppState {
        AppState {
            cfg: Arc::new(config::Config::from_env()),
        }
    }

    /// Both routers assemble — the failure a compile check cannot see (a bad
    /// route shape panics on construction, after a full install).
    #[test]
    fn both_routers_assemble() {
        let _ = build_router(state());
        let _ = hosted_router(state());
    }

    /// A hosted copy refuses the local machine with the sentence — and only
    /// the local machine, and only when hosted.
    #[test]
    fn a_hosted_copy_refuses_the_local_machine_by_name() {
        let err = host::Host::Local
            .refusal(true)
            .expect_err("hosted + local must be refused");
        let text = err.to_string();
        assert_eq!(text, hosted::LOCAL_REFUSED);
        assert!(text.contains("hosted package"), "{text}");
        assert!(
            text.contains("app shape"),
            "names the app-shape remedy: {text}"
        );
        assert!(
            text.contains("/etc/leo-spaces/ssh_key"),
            "names the key: {text}"
        );
        assert!(
            host::Host::Local.refusal(false).is_ok(),
            "app shape drives this box"
        );
        let remote = host::Host::from_settings(Some("10.0.0.9".into()), None, None, None);
        assert!(remote.refusal(true).is_ok(), "hosted drives a remote host");
    }
}
