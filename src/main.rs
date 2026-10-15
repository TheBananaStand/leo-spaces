//! leo-spaces — a hypervisor for Leo hubs and Leo compute machines, and full
//! control of the guests from inside Leo.
//!
//! A Leo *dynamic app package*: the hub builds this binary from git, runs it as
//! a proxied subprocess, and forwards `/p/spaces/*` to it. Leo itself is never
//! recompiled.
//!
//! ## What it does, in layers
//!
//! ```text
//!   facts.rs      what the host is — distro, CPU, IOMMU, GPUs, privilege
//!   provision.rs  what is missing and the command for each gap
//!   spaces.rs     the guests: inventory, lifecycle, define, pass a GPU
//!   ui.rs         the descriptor the Mac and iOS clients render natively
//!   mcp.rs        the same operations as tools, for the model
//! ```
//!
//! Everything privileged goes through one root-owned wrapper with a closed
//! verb set — see `scripts/leo-spaces-admin`. The service account has no
//! blanket sudo, by design, so without that wrapper this package reports and
//! plans but does not apply.
//!
//! ## Subcommands
//!
//! `leo-spaces start` — the HTTP server (the default when no argument is given)
//! `leo-spaces mcp`   — speak MCP over stdio instead
//!
//! Both read the same settings from the same env vars. They share one binary
//! because the hub launches an MCP subprocess with only entitled settings in
//! its environment — not the app's hub-assigned port — so a separate MCP
//! server could not find the HTTP one to call it.

use std::sync::Arc;

use axum::Router;

mod api;
mod auth;
mod config;
mod error;
mod facts;
mod host;
mod mcp;
mod provision;
mod spaces;
mod ui;

/// Shared state. Deliberately small: the host is the source of truth about
/// guests, and anything cached here is a second answer that can disagree with
/// `virsh`.
#[derive(Clone)]
pub struct AppState {
    pub cfg: Arc<config::Config>,
}

/// The whole HTTP surface, assembled.
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
        .route("/leo/ui/descriptor", axum::routing::get(ui::descriptor_handler))
        .route("/leo/ui/data", axum::routing::get(ui::data_handler))
        .with_state(state)
}

#[tokio::main]
async fn main() {
    // stderr in both modes: in MCP mode the host reads stdout as protocol, and
    // a log line there corrupts the stream.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("leo_spaces=info,warn")),
        )
        .init();

    let cfg = Arc::new(config::Config::from_env());

    match std::env::args().nth(1).as_deref() {
        Some("mcp") => {
            eprintln!("leo-spaces: MCP stdio mode, host {}", cfg.host);
            mcp::run(cfg).await;
        }
        // The hub spawns app packages as `<binary> start`. Accept it, and run
        // when invoked bare too, so this is testable by hand.
        _ => serve(cfg).await,
    }
}

async fn serve(cfg: Arc<config::Config>) {
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], cfg.listen_port));
    tracing::info!(host = %cfg.host, "leo-spaces starting");

    let app = build_router(AppState { cfg })
        .layer(tower_http::trace::TraceLayer::new_for_http());

    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("leo-spaces: cannot bind {addr}: {e}");
            std::process::exit(1);
        }
    };
    tracing::info!("listening on http://{addr}");
    if let Err(e) = axum::serve(listener, app).await {
        eprintln!("leo-spaces: server error: {e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The router assembles.
    ///
    /// Worth a test because the failure it guards is the expensive one: a bad
    /// route shape compiles, installs, clones, builds and launches, then
    /// panics on the first line of `main` — so the package is rejected after a
    /// full cold build on the hub. A compile check cannot see it.
    #[test]
    fn the_router_assembles() {
        let state = AppState {
            cfg: Arc::new(config::Config::from_env()),
        };
        let _ = build_router(state);
    }
}
