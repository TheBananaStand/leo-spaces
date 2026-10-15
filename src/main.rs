//! leo-spaces — a hypervisor for Leo hubs and Leo compute machines, and full
//! control of the guests from inside Leo.
//!
//! A Leo *app package*: the hub builds this binary from git and forwards
//! `/p/spaces/*` to it. Leo itself is never recompiled.
//!
//! It declares a **fixed port**, which is what decides how it runs. The hub
//! bubblewrap-jails a dynamic-port (`port: 0`) app package — a namespace with
//! no `/run`, so no `/run/libvirt/libvirt-sock`, so no hypervisor at all — and
//! spawns a fixed-port one bare. A fixed port is therefore not a configuration
//! detail here but the whole reason the HTTP half can work; the cost is that
//! the hub hands a fixed-port app no settings, no data dir and no port, which
//! is what `config.rs` deals with and why the unit carries a package token.
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
//! They share one binary because the hub launches an MCP subprocess with only
//! entitled settings in its environment — not the app's port — so a separate
//! MCP server could not find the HTTP one to call it. The MCP side does get
//! those settings in its environment, where the app side has to ask for them;
//! `config.rs` takes either, so both modes agree on which host they drive.

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

/// Where the hub's Studio log pane reads an app package's output from, and so
/// where this writes it.
///
/// The hub pipes a *dynamic*-port app's stdout and stderr into
/// `{data_dir}/apps/<name>/logs/app.log` and sends a fixed-port one's to
/// `/dev/null` — nothing in the journal either, because the hub is the parent
/// and it discards both streams. A hypervisor manager whose whole job is
/// reporting what a box is, with no log anywhere, is not debuggable, so this
/// opens that same file itself: the pane keeps working and the path stays the
/// one place to look.
///
/// Derived from the running binary (`…/apps/spaces/src/target/release/…`)
/// rather than from `LEO_DATA_DIR`, which is part of the hand-off a fixed-port
/// app does not get. `LEO_SPACES_LOG` overrides, and a by-hand run that
/// matches neither simply logs to stderr.
fn log_file() -> Option<std::fs::File> {
    let path = match std::env::var("LEO_SPACES_LOG").ok().filter(|v| !v.is_empty()) {
        Some(p) => std::path::PathBuf::from(p),
        None => {
            let exe = std::env::current_exe().ok()?;
            // …/apps/<name>/src/target/release/leo-spaces → …/apps/<name>
            let app_dir = exe
                .ancestors()
                .find(|a| a.file_name().is_some_and(|n| n == "src"))
                .and_then(|src| src.parent())
                .filter(|app| {
                    app.parent()
                        .and_then(|p| p.file_name())
                        .is_some_and(|n| n == "apps")
                })?;
            app_dir.join("logs").join("app.log")
        }
    };
    std::fs::create_dir_all(path.parent()?).ok()?;
    // Truncated per launch, as the hub does it, so it stays bounded without a
    // rotation story.
    std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&path)
        .ok()
}

#[tokio::main]
async fn main() {
    // Never stdout: in MCP mode the host reads it as protocol and a log line
    // there corrupts the stream. stderr, plus the hub's own app.log when this
    // is running as the installed app package — see `log_file`.
    let to_file = match std::env::args().nth(1).as_deref() {
        // MCP mode is a subprocess of the hub with its stderr attached, so it
        // is already visible; a second writer would only split the record.
        Some("mcp") => None,
        _ => log_file(),
    };
    let filter = || {
        tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("leo_spaces=info,warn"))
    };
    match to_file {
        Some(f) => tracing_subscriber::fmt()
            .with_ansi(false)
            .with_writer(move || -> Box<dyn std::io::Write> {
                match f.try_clone() {
                    Ok(f) => Box::new(f),
                    Err(_) => Box::new(std::io::stderr()),
                }
            })
            .with_env_filter(filter())
            .init(),
        None => tracing_subscriber::fmt()
            .with_writer(std::io::stderr)
            .with_env_filter(filter())
            .init(),
    }

    let cfg = Arc::new(config::Config::load().await);

    match std::env::args().nth(1).as_deref() {
        Some("mcp") => {
            eprintln!("leo-spaces: MCP stdio mode, host {}", cfg.host);
            mcp::run(cfg).await;
        }
        // The hub spawns app packages as `<binary> start`, and so does the
        // unit `leo packages service provision` writes. Accept it, and run
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
