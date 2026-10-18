//! REST surface. The hub mounts this whole router at `/p/spaces/*` and strips
//! the prefix before forwarding, so everything here is rooted at `/`.
//!
//! Auth levels match what the operation costs. Reads are session-level: any
//! authenticated person may see what the host is and what is running on it.
//! Everything that changes a host or a guest is `AdminCaller` — defining a VM,
//! handing it a GPU, and editing how the box boots are administration.

use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::auth::{AdminCaller, Caller};
use crate::error::Error;
use crate::{facts, provision, spaces, AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/health", get(health))
        .route("/api/host", get(host))
        .route("/api/host/plan", get(plan))
        .route("/api/host/apply", post(apply))
        .route("/api/host/installers", get(installers))
        .route("/api/host/fetch-iso", post(fetch_iso))
        .route("/api/host/import-iso", post(import_iso))
        .route("/api/spaces", get(list).post(create))
        .route("/api/spaces/{name}", axum::routing::delete(remove))
        .route("/api/spaces/{name}/control", post(control))
        .route("/api/spaces/{name}/gpu", post(gpu))
        .route("/api/spaces/{name}/snapshot", post(snapshot))
        .route("/api/spaces/{name}/guest-tools", post(guest_tools))
        .route("/api/spaces/{name}/console/ws", get(crate::console::socket))
}

/// Liveness, no auth. The hub polls this to decide whether the subprocess came
/// up, so it must answer before any host probe is attempted — a hypervisor
/// host that is down must not make this package look dead.
async fn health() -> Json<Value> {
    Json(json!({ "ok": true, "package": "spaces" }))
}

async fn host(_c: Caller, State(s): State<AppState>) -> Result<Json<Value>, Error> {
    let f = facts::probe(&s.cfg.host).await?;
    Ok(Json(serde_json::to_value(f).unwrap_or_default()))
}

async fn plan(_c: Caller, State(s): State<AppState>) -> Result<Json<Value>, Error> {
    let f = facts::probe(&s.cfg.host).await?;
    Ok(Json(
        serde_json::to_value(provision::plan(&f)).unwrap_or_default(),
    ))
}

#[derive(Deserialize)]
struct ApplyBody {
    /// A step id from the current plan. Never a command.
    step: String,
}

/// Apply one provisioning step.
///
/// The body names a *step*, and the command that runs is the one the
/// provisioner generated for the host as it is right now. There is no path
/// from caller text to a shell: an unknown id is a 404 rather than anything
/// being executed.
async fn apply(
    _c: AdminCaller,
    State(s): State<AppState>,
    Json(body): Json<ApplyBody>,
) -> Result<Json<Value>, Error> {
    let f = facts::probe(&s.cfg.host).await?;
    let p = provision::plan(&f);

    let step = provision::step(&p, &body.step)
        .ok_or_else(|| Error::Other(format!("no provisioning step called '{}'", body.step)))?;

    if step.satisfied {
        return Ok(Json(json!({
            "ok": true, "step": step.id, "output": "already satisfied, nothing to do"
        })));
    }
    if let Some(reason) = &step.blocked {
        return Err(Error::Other(format!(
            "'{}' cannot run here: {reason}",
            step.id
        )));
    }
    let unmet = provision::unmet_requirements(&p, step);
    if !unmet.is_empty() {
        // Refusing rather than attempting: `virsh net-start` before libvirt
        // exists fails in a way that reads like a broken network, not a
        // missed step.
        return Err(Error::Other(format!(
            "'{}' needs {} first",
            step.id,
            unmet.join(", ")
        )));
    }
    let Some(command) = &step.command else {
        return Err(Error::Other(format!(
            "'{}' has nothing to run on this host",
            step.id
        )));
    };

    let out = s.cfg.host.run(command).await?;
    if !out.ok() {
        return Err(Error::Other(format!(
            "{} failed: {}",
            step.id,
            out.failure()
        )));
    }
    Ok(Json(json!({
        "ok": true,
        "step": step.id,
        "needs_reboot": step.needs_reboot,
        "output": out.trimmed(),
    })))
}

/// Every OS the catalog offers, with what the host has of its installer.
async fn installers(_c: Caller, State(s): State<AppState>) -> Result<Json<Value>, Error> {
    let f = facts::probe(&s.cfg.host).await?;
    Ok(Json(
        serde_json::to_value(crate::installers::list(&f)).unwrap_or_default(),
    ))
}

#[derive(Deserialize)]
struct FetchIsoBody {
    /// A catalog id. Never an address: the helper holds the addresses, and a
    /// request can only choose among them.
    os: String,
}

/// Start downloading one catalog installer onto the host. Returns at once;
/// the Installers panel shows the download's progress and its failure.
async fn fetch_iso(
    _c: AdminCaller,
    State(s): State<AppState>,
    Json(body): Json<FetchIsoBody>,
) -> Result<Json<Value>, Error> {
    let os = crate::catalog::find(&body.os)
        .ok_or_else(|| Error::Other(format!("'{}' is not in the OS catalog", body.os)))?;
    if os.source.is_none() {
        return Err(Error::Other(format!(
            "{} is licensed, so it is not downloaded — put the image at {}",
            os.label,
            os.iso_path()
        )));
    }
    let f = facts::probe(&s.cfg.host).await?;
    let Some(command) = f.privilege.wrap(&format!("fetch-iso {}", os.id)) else {
        return Err(Error::Other(provision::privilege_handover(&f)));
    };
    let out = s.cfg.host.run(&command).await?;
    if !out.ok() {
        return Err(Error::Other(format!(
            "fetch-iso {} failed: {}",
            os.id,
            out.failure()
        )));
    }
    Ok(Json(
        json!({ "ok": true, "os": os.id, "output": out.trimmed() }),
    ))
}

/// Adopt an installer that was fetched as leo into the import directory —
/// the way a licensed image reaches the root-owned media directory.
async fn import_iso(
    _c: AdminCaller,
    State(s): State<AppState>,
    Json(body): Json<FetchIsoBody>,
) -> Result<Json<Value>, Error> {
    let os = crate::catalog::find(&body.os)
        .ok_or_else(|| Error::Other(format!("'{}' is not in the OS catalog", body.os)))?;
    let f = facts::probe(&s.cfg.host).await?;
    let Some(command) = f.privilege.wrap(&format!("import-iso {}", os.id)) else {
        return Err(Error::Other(provision::privilege_handover(&f)));
    };
    let out = s.cfg.host.run(&command).await?;
    if !out.ok() {
        return Err(Error::Other(format!(
            "import-iso {} failed: {}",
            os.id,
            out.failure()
        )));
    }
    Ok(Json(
        json!({ "ok": true, "os": os.id, "output": out.trimmed() }),
    ))
}

async fn list(_c: Caller, State(s): State<AppState>) -> Result<Json<Value>, Error> {
    let list = spaces::list(&s.cfg.host).await?;
    Ok(Json(serde_json::to_value(list).unwrap_or_default()))
}

async fn create(
    _c: AdminCaller,
    State(s): State<AppState>,
    Json(req): Json<spaces::NewSpace>,
) -> Result<Json<Value>, Error> {
    let f = facts::probe(&s.cfg.host).await?;
    let created = spaces::create(
        &s.cfg.host,
        &f,
        &req,
        s.cfg.guest_hub.as_deref(),
        &s.cfg.leo_api_url,
    )
    .await?;
    // The approval comes from the sheet's next action, with the caller's own
    // session; this waits for it in the background and puts the token on
    // the CD. See `guest_tools::finish_pairing`.
    if let (Some(plan), false) = (created.pairing.clone(), created.pairing_code.is_empty()) {
        tokio::spawn(crate::guest_tools::finish_pairing(
            s.cfg.host.clone(),
            f,
            req.name.clone(),
            created.hub.clone(),
            plan,
            s.cfg.leo_api_url.clone(),
            created.pairing_code.clone(),
        ));
    }
    Ok(Json(json!({
        "ok": true,
        "message": created.message,
        "pairing_code": created.pairing_code,
        "admin_password": created.admin_password,
    })))
}

/// Build the guest tools CD for an existing space and attach it.
async fn guest_tools(
    _c: AdminCaller,
    State(s): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<Value>, Error> {
    let f = facts::probe(&s.cfg.host).await?;
    let msg =
        spaces::attach_guest_tools(&s.cfg.host, &f, &name, s.cfg.guest_hub.as_deref()).await?;
    Ok(Json(json!({ "ok": true, "message": msg })))
}

#[derive(Deserialize)]
struct RemoveBody {
    /// Keeping the disk is the default for a reason: a 120 GB Windows install
    /// with somebody's work on it is not something to delete as a side effect
    /// of removing a VM definition.
    #[serde(default = "yes")]
    keep_disk: bool,
}

fn yes() -> bool {
    true
}

async fn remove(
    _c: AdminCaller,
    State(s): State<AppState>,
    Path(name): Path<String>,
    body: Option<Json<RemoveBody>>,
) -> Result<Json<Value>, Error> {
    let keep = body.map(|b| b.keep_disk).unwrap_or(true);
    let msg = spaces::delete(&s.cfg.host, &name, keep).await?;
    Ok(Json(json!({ "ok": true, "message": msg })))
}

#[derive(Deserialize)]
struct ControlBody {
    action: String,
}

async fn control(
    _c: AdminCaller,
    State(s): State<AppState>,
    Path(name): Path<String>,
    Json(body): Json<ControlBody>,
) -> Result<Json<Value>, Error> {
    let action = spaces::Action::parse(&body.action)
        .ok_or_else(|| Error::Other(format!("'{}' is not a space action", body.action)))?;
    spaces::control(&s.cfg.host, &name, action).await?;
    Ok(Json(
        json!({ "ok": true, "message": format!("{name}: {}", body.action) }),
    ))
}

#[derive(Deserialize)]
struct GpuBody {
    /// A PCI address to assign, or null to release whatever is assigned.
    gpu: Option<String>,
}

async fn gpu(
    _c: AdminCaller,
    State(s): State<AppState>,
    Path(name): Path<String>,
    Json(body): Json<GpuBody>,
) -> Result<Json<Value>, Error> {
    let f = facts::probe(&s.cfg.host).await?;
    let msg = spaces::set_gpu(&s.cfg.host, &f, &name, body.gpu.as_deref()).await?;
    Ok(Json(json!({ "ok": true, "message": msg })))
}

#[derive(Deserialize)]
struct SnapshotBody {
    label: String,
}

async fn snapshot(
    _c: AdminCaller,
    State(s): State<AppState>,
    Path(name): Path<String>,
    Json(body): Json<SnapshotBody>,
) -> Result<Json<Value>, Error> {
    let msg = spaces::snapshot(&s.cfg.host, &name, &body.label).await?;
    Ok(Json(json!({ "ok": true, "message": msg })))
}
