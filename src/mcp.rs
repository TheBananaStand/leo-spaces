//! MCP stdio server for `leo-spaces mcp`.
//!
//! Why this lives in the same binary as the HTTP server: the hub launches an
//! MCP subprocess with only its entitled settings injected as environment
//! variables, and that environment does **not** include the app's hub-assigned
//! port. A separate MCP server could not discover the HTTP server to call it.
//! Sharing the process image lets MCP mode build the same `Host` from the same
//! env vars and reuse every function — no network hop, no port to look up.
//!
//! Wire format: JSON-RPC 2.0, newline-delimited, over stdin/stdout. Only
//! stdout carries protocol; every log line goes to stderr so the host can tell
//! them apart without parsing.
//!
//! ## Two tools, split along the line that matters
//!
//! `spaces` reads and runs guests — the everyday surface. `spaces_host` changes
//! the machine underneath them: installing a hypervisor, editing the kernel
//! command line, binding a GPU to vfio. They are separate tools rather than
//! one with fifteen actions because the hub can restrict a tool, and "may look
//! at the machines" and "may change how the box boots" are different grants.

use std::sync::Arc;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::config::Config;
use crate::{facts, provision, spaces};

const PROTOCOL_VERSION: &str = "2024-11-05";

// ── Schemas ──────────────────────────────────────────────────────────────────

fn spaces_tool() -> Value {
    json!({
        "name": "spaces",
        "description": "Virtual machines on a Leo hub or compute machine — list them, read one, start and stop them, create and delete them, and assign a GPU. A 'space' is one guest.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": [
                        "list", "get",
                        "start", "shutdown", "restart", "force_off", "pause", "resume",
                        "autostart_on", "autostart_off",
                        "create", "delete",
                        "assign_gpu", "release_gpu",
                        "snapshot", "restore"
                    ],
                    "description": "What to do"
                },
                "name": { "type": "string", "description": "The space's name. Required for everything except list and create-with-defaults." },
                "os": { "type": "string", "description": "For create: an id from spaces_host installers — win2k22 (default; Windows Server licenses several concurrent sessions), win2k25, win2k19, win11, win10, ubuntu24.04, ubuntu26.04, debian13, fedora43, archlinux, rocky10, rocky9, almalinux10, opensuse16.0, opensusetumbleweed, freebsd15.0, freebsd14.3. Its installer has to be on the host first." },
                "vcpus": { "type": "integer", "description": "For create: virtual CPUs. Default 8." },
                "memory_mb": { "type": "integer", "description": "For create: RAM in MB. Default 16384." },
                "disk_gb": { "type": "integer", "description": "For create: disk size in GB. Default 120." },
                "iso": { "type": "string", "description": "For create: path on the host to an installer image." },
                "product_key": { "type": "string", "description": "For create, Windows only: a product key. Blank installs with a generic key and activates later." },
                "gpu": { "type": "string", "description": "PCI address of a GPU, e.g. 0000:01:00.0. For create and assign_gpu." },
                "label": { "type": "string", "description": "For snapshot and restore: the snapshot name." },
                "keep_disk": { "type": "boolean", "description": "For delete: keep the disk image. Defaults to true — a Windows install with somebody's work on it is not deleted as a side effect." }
            },
            "required": ["action"]
        }
    })
}

fn host_tool() -> Value {
    json!({
        "name": "spaces_host",
        "description": "The machine the guests run on: what it is, what a hypervisor still needs, and applying one setup step. Reads are free; applying changes the host, and the steps that edit the kernel command line need a reboot.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["status", "plan", "apply", "gpus", "installers", "fetch_iso", "import_iso"],
                    "description": "status: what the host is. plan: every setup step and whether it is satisfied. apply: run one step by id. gpus: graphics devices and whether each can be given to a guest. installers: every operating system a space can be made from, and whether its installer image is on the host. fetch_iso: start downloading one (Linux, BSD and Windows Server evaluation media). import_iso: adopt an image somebody put at /var/lib/leo/spaces-import/<id>.iso on the host — how a licensed Windows 10 or 11 ISO gets in."
                },
                "step": { "type": "string", "description": "For apply: a step id from plan, e.g. packages, daemon, network, storage, group, virtio, iommu, or vfio:0000:01:00.0." },
                "os": { "type": "string", "description": "For fetch_iso: a catalog id from installers, e.g. ubuntu24.04, debian13, archlinux." }
            },
            "required": ["action"]
        }
    })
}

// ── Validation ───────────────────────────────────────────────────────────────

/// Check the required fields for a (tool, action) pair before any host call.
///
/// Pure, so the whole matrix is testable without a hypervisor. A missing field
/// caught here produces a message naming the field; caught later it produces a
/// libvirt error about a domain called "", which sends whoever is reading it
/// looking for a domain.
pub fn validate(tool: &str, action: &str, args: &Value) -> Result<(), String> {
    let required: &[&str] = match (tool, action) {
        ("spaces", "list") => &[],
        ("spaces", "create") => &["name"],
        ("spaces", "snapshot" | "restore") => &["name", "label"],
        ("spaces", "assign_gpu") => &["name", "gpu"],
        // Named explicitly rather than caught by `_`. A catch-all here
        // demanded `name` for every unrecognised action too, so a typo'd
        // action came back as "requires 'name'" — sending the reader off to
        // supply a field that was never the problem, instead of being told
        // the action does not exist. Unknown actions fall through to Ok and
        // are named by the dispatch.
        (
            "spaces",
            "get" | "start" | "shutdown" | "restart" | "reboot" | "force_off" | "destroy" | "pause"
            | "suspend" | "resume" | "autostart_on" | "autostart_off" | "delete" | "release_gpu",
        ) => &["name"],
        ("spaces_host", "apply") => &["step"],
        ("spaces_host", "fetch_iso" | "import_iso") => &["os"],
        _ => &[],
    };

    for &field in required {
        match args.get(field) {
            None | Some(Value::Null) => {
                return Err(format!("action '{action}' requires '{field}'"));
            }
            Some(Value::String(s)) if s.trim().is_empty() => {
                return Err(format!("action '{action}' requires a non-empty '{field}'"));
            }
            _ => {}
        }
    }
    Ok(())
}

// ── JSON-RPC plumbing ────────────────────────────────────────────────────────

fn ok_response(id: &Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn error_response(id: &Value, code: i32, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn tool_ok(text: impl Into<String>) -> Value {
    json!({ "content": [{ "type": "text", "text": text.into() }] })
}

fn tool_err(text: impl Into<String>) -> Value {
    json!({ "content": [{ "type": "text", "text": text.into() }], "isError": true })
}

fn as_json(v: &impl serde::Serialize) -> String {
    serde_json::to_string_pretty(v).unwrap_or_else(|_| "{}".into())
}

// ── Dispatch ─────────────────────────────────────────────────────────────────

/// Route one `tools/call`.
///
/// `Ok(value)` is an MCP result, whether or not the operation succeeded — a
/// libvirt refusal comes back as `isError: true` content, because it is output
/// the model should read and act on. `Err` is reserved for a malformed request,
/// which the caller maps to a JSON-RPC error instead.
async fn dispatch(cfg: &Config, tool: &str, args: &Value) -> Result<Value, String> {
    let action = args
        .get("action")
        .and_then(Value::as_str)
        .ok_or_else(|| "missing required field 'action'".to_string())?;

    validate(tool, action, args)?;

    let s = |k: &str| {
        args.get(k)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let host = &cfg.host;

    match (tool, action) {
        // ── spaces_host ──────────────────────────────────────────────────────
        ("spaces_host", "status") => match facts::probe(host).await {
            Ok(f) => Ok(tool_ok(as_json(&f))),
            Err(e) => Ok(tool_err(format!("could not read the host: {e}"))),
        },

        ("spaces_host", "gpus") => match facts::probe(host).await {
            Ok(f) => {
                // Each card with its blockers spelled out, because "can this
                // be passed through" is the only question asked of this list
                // and a bare inventory does not answer it.
                let cards: Vec<Value> = f
                    .gpus
                    .iter()
                    .map(|g| {
                        json!({
                            "address": g.address,
                            "ids": g.ids,
                            "name": g.name,
                            "driver": g.driver,
                            "iommu_group": g.iommu_group,
                            "shares_group_with": g.group_siblings,
                            "integrated": g.integrated,
                            "host_display": g.is_boot_vga,
                            "can_pass_through": g.passthrough_blockers(&f.gpus).is_empty(),
                            "blockers": g.passthrough_blockers(&f.gpus),
                        })
                    })
                    .collect();
                Ok(tool_ok(as_json(&json!({
                    "iommu_enabled": f.iommu_enabled(),
                    "iommu_groups": f.iommu_groups,
                    "gpus": cards
                }))))
            }
            Err(e) => Ok(tool_err(format!("could not read the host: {e}"))),
        },

        ("spaces_host", "plan") => match facts::probe(host).await {
            Ok(f) => Ok(tool_ok(as_json(&provision::plan(&f)))),
            Err(e) => Ok(tool_err(format!("could not read the host: {e}"))),
        },

        ("spaces_host", "apply") => {
            let id = s("step");
            let f = match facts::probe(host).await {
                Ok(f) => f,
                Err(e) => return Ok(tool_err(format!("could not read the host: {e}"))),
            };
            let plan = provision::plan(&f);
            let Some(step) = provision::step(&plan, &id) else {
                // List what there is rather than just refusing: the model
                // guessed an id and the correct set is short.
                let known: Vec<&str> = plan.steps.iter().map(|s| s.id.as_str()).collect();
                return Ok(tool_err(format!(
                    "no step called '{id}'. Steps on this host: {}",
                    known.join(", ")
                )));
            };
            if step.satisfied {
                return Ok(tool_ok(format!("'{id}' is already satisfied")));
            }
            if let Some(reason) = &step.blocked {
                return Ok(tool_err(format!("'{id}' cannot run here: {reason}")));
            }
            let unmet = provision::unmet_requirements(&plan, step);
            if !unmet.is_empty() {
                return Ok(tool_err(format!(
                    "'{id}' needs {} applied first",
                    unmet.join(", ")
                )));
            }
            let Some(cmd) = &step.command else {
                return Ok(tool_err(format!("'{id}' has nothing to run on this host")));
            };
            match host.run(cmd).await {
                Ok(out) if out.ok() => Ok(tool_ok(format!(
                    "{id}: done.{}\n{}",
                    if step.needs_reboot {
                        " A reboot is needed for this to take effect."
                    } else {
                        ""
                    },
                    out.trimmed()
                ))),
                Ok(out) => Ok(tool_err(format!("{id} failed: {}", out.failure()))),
                Err(e) => Ok(tool_err(format!("{id} could not run: {e}"))),
            }
        }

        ("spaces_host", "installers") => match facts::probe(host).await {
            Ok(f) => Ok(tool_ok(as_json(&crate::installers::list(&f)))),
            Err(e) => Ok(tool_err(format!("could not read the host: {e}"))),
        },

        ("spaces_host", "fetch_iso") => {
            let id = s("os");
            let Some(os) = crate::catalog::find(&id) else {
                return Ok(tool_err(format!(
                    "'{id}' is not in the OS catalog. One of: {}",
                    crate::catalog::ids().join(", ")
                )));
            };
            if os.source.is_none() {
                return Ok(tool_err(format!(
                    "{} is licensed, so it is not downloaded — the image goes at {}",
                    os.label,
                    os.iso_path()
                )));
            }
            let f = match facts::probe(host).await {
                Ok(f) => f,
                Err(e) => return Ok(tool_err(format!("could not read the host: {e}"))),
            };
            let Some(cmd) = f.privilege.wrap(&format!("fetch-iso {}", os.id)) else {
                return Ok(tool_err(provision::privilege_handover(&f)));
            };
            match host.run(&cmd).await {
                Ok(out) if out.ok() => Ok(tool_ok(out.trimmed().to_string())),
                Ok(out) => Ok(tool_err(format!(
                    "fetch-iso {} failed: {}",
                    os.id,
                    out.failure()
                ))),
                Err(e) => Ok(tool_err(format!("fetch-iso could not run: {e}"))),
            }
        }

        ("spaces_host", "import_iso") => {
            let id = s("os");
            let Some(os) = crate::catalog::find(&id) else {
                return Ok(tool_err(format!(
                    "'{id}' is not in the OS catalog. One of: {}",
                    crate::catalog::ids().join(", ")
                )));
            };
            let f = match facts::probe(host).await {
                Ok(f) => f,
                Err(e) => return Ok(tool_err(format!("could not read the host: {e}"))),
            };
            let Some(cmd) = f.privilege.wrap(&format!("import-iso {}", os.id)) else {
                return Ok(tool_err(provision::privilege_handover(&f)));
            };
            match host.run(&cmd).await {
                Ok(out) if out.ok() => Ok(tool_ok(out.trimmed().to_string())),
                Ok(out) => Ok(tool_err(format!(
                    "import-iso {} failed: {}",
                    os.id,
                    out.failure()
                ))),
                Err(e) => Ok(tool_err(format!("import-iso could not run: {e}"))),
            }
        }

        // ── spaces ───────────────────────────────────────────────────────────
        ("spaces", "list") => match spaces::list(host).await {
            Ok(list) => Ok(tool_ok(as_json(&list))),
            Err(e) => Ok(tool_err(e.to_string())),
        },

        ("spaces", "get") => match spaces::get(host, &s("name")).await {
            Ok(sp) => Ok(tool_ok(as_json(&sp))),
            Err(e) => Ok(tool_err(e.to_string())),
        },

        ("spaces", "create") => {
            let f = match facts::probe(host).await {
                Ok(f) => f,
                Err(e) => return Ok(tool_err(format!("could not read the host: {e}"))),
            };
            if !f.ready_for_guests() {
                return Ok(tool_err(format!(
                    "this host cannot run guests yet ({}). Call spaces_host plan to see what is missing.",
                    f.status_label()
                )));
            }
            // Deserialised from the same struct the REST route uses, so the
            // defaults are the same on both paths rather than written twice.
            let req: spaces::NewSpace = match serde_json::from_value(args.clone()) {
                Ok(r) => r,
                Err(e) => return Ok(tool_err(format!("cannot read that request: {e}"))),
            };
            match spaces::create(host, &f, &req, cfg.guest_hub.as_deref(), &cfg.leo_api_url).await {
                Ok(created) => {
                    if let (Some(plan), false) =
                        (created.pairing.clone(), created.pairing_code.is_empty())
                    {
                        tokio::spawn(crate::guest_tools::finish_pairing(
                            host.clone(),
                            f,
                            req.name.clone(),
                            created.hub.clone(),
                            plan,
                            cfg.leo_api_url.clone(),
                            created.pairing_code.clone(),
                        ));
                    }
                    Ok(tool_ok(format!(
                        "{}\nIt is defined but not started — call spaces start when you are ready.{}{}",
                        created.message,
                        created.admin_password.as_deref().map(|p| format!("\nLocal admin: leo / {p}")).unwrap_or_default(),
                        if created.pairing_code.is_empty() { String::new() } else {
                            format!("\nIts satellite is waiting on pairing code {} — an admin approves it in Settings → Devices (the app does this itself when a person clicks Create).", created.pairing_code)
                        }
                    )))
                }
                Err(e) => Ok(tool_err(e.to_string())),
            }
        }

        ("spaces", "delete") => {
            // Default true. A 120 GB Windows install is not deleted as a side
            // effect of removing a VM definition, and the model has to say so
            // explicitly to lose it.
            let keep = args
                .get("keep_disk")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            match spaces::delete(host, &s("name"), keep).await {
                Ok(msg) => Ok(tool_ok(msg)),
                Err(e) => Ok(tool_err(e.to_string())),
            }
        }

        ("spaces", "assign_gpu") | ("spaces", "release_gpu") => {
            let f = match facts::probe(host).await {
                Ok(f) => f,
                Err(e) => return Ok(tool_err(format!("could not read the host: {e}"))),
            };
            let gpu = if action == "assign_gpu" {
                Some(s("gpu"))
            } else {
                None
            };
            match spaces::set_gpu(host, &f, &s("name"), gpu.as_deref()).await {
                Ok(msg) => Ok(tool_ok(msg)),
                Err(e) => Ok(tool_err(e.to_string())),
            }
        }

        ("spaces", "snapshot") => match spaces::snapshot(host, &s("name"), &s("label")).await {
            Ok(msg) => Ok(tool_ok(msg)),
            Err(e) => Ok(tool_err(e.to_string())),
        },

        ("spaces", "restore") => match spaces::restore(host, &s("name"), &s("label")).await {
            Ok(msg) => Ok(tool_ok(msg)),
            Err(e) => Ok(tool_err(e.to_string())),
        },

        // Every remaining `spaces` action is a run-state change, and they all
        // go through the same call. Mapped by name rather than listed again so
        // the enum in the schema is the only place the set is written.
        ("spaces", other) => {
            let Some(act) = spaces::Action::parse(other) else {
                return Err(format!("unknown action '{other}' for tool 'spaces'"));
            };
            match spaces::control(host, &s("name"), act).await {
                Ok(_) => Ok(tool_ok(format!("{}: {other}", s("name")))),
                Err(e) => Ok(tool_err(e.to_string())),
            }
        }

        _ => Err(format!("unknown action '{action}' for tool '{tool}'")),
    }
}

// ── Main loop ────────────────────────────────────────────────────────────────

/// Run the MCP stdio loop until stdin closes.
///
/// One JSON object per line in, one per line out. Notifications — requests
/// with no `id` — are dropped: the spec says a server must not answer them,
/// and there is nothing here to act on.
pub async fn run(cfg: Arc<Config>) {
    let stdin = BufReader::new(tokio::io::stdin());
    let mut stdout = tokio::io::stdout();
    let mut lines = stdin.lines();

    loop {
        let line = match lines.next_line().await {
            Ok(Some(l)) if l.trim().is_empty() => continue,
            Ok(Some(l)) => l,
            Ok(None) => break,
            Err(e) => {
                eprintln!("mcp: stdin read error: {e}");
                break;
            }
        };

        let msg: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                // A parse error before there is an id: answer with id=null,
                // per spec.
                let resp = error_response(&Value::Null, -32700, &format!("parse error: {e}"));
                let _ = stdout.write_all(format!("{resp}\n").as_bytes()).await;
                continue;
            }
        };

        let id = match msg.get("id") {
            Some(id) if !id.is_null() => id.clone(),
            _ => continue,
        };

        let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
        let resp = handle_request(&cfg, &id, method, &msg).await;
        let _ = stdout.write_all(format!("{resp}\n").as_bytes()).await;
    }
}

async fn handle_request(cfg: &Config, id: &Value, method: &str, msg: &Value) -> Value {
    match method {
        "initialize" => ok_response(
            id,
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "leo-spaces-mcp", "version": env!("CARGO_PKG_VERSION") }
            }),
        ),

        "tools/list" => ok_response(id, json!({ "tools": [spaces_tool(), host_tool()] })),

        "tools/call" => {
            let params = msg.get("params").unwrap_or(&Value::Null);
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let args = params.get("arguments").unwrap_or(&Value::Null);

            if name != "spaces" && name != "spaces_host" {
                return ok_response(id, tool_err(format!("unknown tool: '{name}'")));
            }
            match dispatch(cfg, name, args).await {
                Ok(result) => ok_response(id, result),
                // Malformed requests only — a host failure comes back as
                // Ok(tool_err(…)). Mapping these to isError results rather
                // than JSON-RPC errors keeps them readable as tool output.
                Err(message) => ok_response(id, tool_err(message)),
            }
        }

        _ => error_response(id, -32601, &format!("method not found: '{method}'")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> Config {
        Config::from_env()
    }

    /// Both tools must be advertised, with their full action enums. A missing
    /// tool is a silent capability drop — the host never learns it exists.
    #[test]
    fn tools_list_advertises_both_tools_and_their_actions() {
        let sp = spaces_tool();
        assert_eq!(sp["name"], "spaces");
        let actions: Vec<&str> = sp["inputSchema"]["properties"]["action"]["enum"]
            .as_array()
            .expect("an action enum")
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        for want in [
            "list",
            "get",
            "start",
            "shutdown",
            "restart",
            "force_off",
            "create",
            "delete",
            "assign_gpu",
            "release_gpu",
            "snapshot",
            "restore",
        ] {
            assert!(actions.contains(&want), "spaces is missing '{want}'");
        }
        // Every advertised action must also get past the validator with its
        // required fields supplied — a schema the validator rejects is a
        // capability the model is told about and then refused.
        for a in &actions {
            let args = json!({
                "action": a, "name": "win-cad", "label": "before-run",
                "gpu": "0000:01:00.0", "step": "packages"
            });
            assert!(
                validate("spaces", a, &args).is_ok(),
                "'{a}' is advertised but the validator refuses a fully-populated call"
            );
        }

        let h = host_tool();
        assert_eq!(h["name"], "spaces_host");
        let hactions: Vec<&str> = h["inputSchema"]["properties"]["action"]["enum"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(
            hactions,
            vec![
                "status",
                "plan",
                "apply",
                "gpus",
                "installers",
                "fetch_iso",
                "import_iso"
            ]
        );
    }

    /// Every run-state action in the schema must actually parse. The dispatch
    /// falls through to `Action::parse` for all of them, so one advertised and
    /// unparseable is a tool call that answers "unknown action" — and the
    /// schema is where the model got the name.
    #[test]
    fn every_advertised_run_state_action_parses() {
        let sp = spaces_tool();
        let actions: Vec<String> = sp["inputSchema"]["properties"]["action"]["enum"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        // The ones dispatch handles explicitly rather than through Action.
        let handled = [
            "list",
            "get",
            "create",
            "delete",
            "assign_gpu",
            "release_gpu",
            "snapshot",
            "restore",
        ];
        for a in &actions {
            if handled.contains(&a.as_str()) {
                continue;
            }
            assert!(
                spaces::Action::parse(a).is_some(),
                "'{a}' is advertised but Action::parse does not know it"
            );
        }
    }

    /// A missing required field is named, before any host call.
    #[test]
    fn missing_fields_are_named_not_discovered_later() {
        let e = validate("spaces", "start", &json!({ "action": "start" })).unwrap_err();
        assert!(e.contains("name"), "{e}");

        let e = validate(
            "spaces",
            "snapshot",
            &json!({ "action": "snapshot", "name": "x" }),
        )
        .unwrap_err();
        assert!(e.contains("label"), "{e}");

        let e = validate(
            "spaces",
            "assign_gpu",
            &json!({ "action": "assign_gpu", "name": "x" }),
        )
        .unwrap_err();
        assert!(e.contains("gpu"), "{e}");

        let e = validate("spaces_host", "apply", &json!({ "action": "apply" })).unwrap_err();
        assert!(e.contains("step"), "{e}");

        // An empty string is not a value. A domain called "" produces a
        // libvirt error about a domain, which sends the reader looking for one.
        let e = validate(
            "spaces",
            "start",
            &json!({ "action": "start", "name": "   " }),
        )
        .unwrap_err();
        assert!(e.contains("non-empty"), "{e}");

        // Reads need nothing.
        assert!(validate("spaces", "list", &json!({ "action": "list" })).is_ok());
        assert!(validate("spaces_host", "status", &json!({ "action": "status" })).is_ok());
        assert!(validate("spaces_host", "plan", &json!({ "action": "plan" })).is_ok());
    }

    /// An unknown tool is an isError result, not a panic and not a silent
    /// empty success — which the host would read as "it ran and found nothing".
    #[tokio::test]
    async fn an_unknown_tool_is_an_error_result() {
        let msg = json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": "does_not_exist", "arguments": { "action": "list" } }
        });
        let r = handle_request(&cfg(), &json!(1), "tools/call", &msg).await;
        assert!(r["result"]["isError"].as_bool().unwrap_or(false), "{r}");
        assert!(r["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("does_not_exist"));
    }

    /// An unrecognised method is a JSON-RPC error, because the host may gate
    /// retries on the code.
    #[tokio::test]
    async fn an_unknown_method_is_method_not_found() {
        let msg = json!({ "jsonrpc": "2.0", "id": 42, "method": "not/a/method" });
        let r = handle_request(&cfg(), &json!(42), "not/a/method", &msg).await;
        assert_eq!(r["error"]["code"], -32601, "{r}");
    }

    #[tokio::test]
    async fn initialize_states_the_protocol_version_and_names_the_server() {
        let msg = json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize" });
        let r = handle_request(&cfg(), &json!(1), "initialize", &msg).await;
        assert_eq!(r["result"]["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(r["result"]["serverInfo"]["name"], "leo-spaces-mcp");
        assert!(r["result"]["capabilities"]["tools"].is_object());
    }

    /// A call with no `action` must say so rather than dispatching to a
    /// default, which would make "spaces" with an empty body do something.
    #[tokio::test]
    async fn a_call_with_no_action_is_refused() {
        let msg = json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": "spaces", "arguments": {} }
        });
        let r = handle_request(&cfg(), &json!(1), "tools/call", &msg).await;
        assert!(r["result"]["isError"].as_bool().unwrap_or(false));
        assert!(r["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("action"));
    }

    /// An unrecognised action is reported as unrecognised — not as a missing
    /// field. The validator used to demand `name` for anything it did not
    /// know, so a typo'd action sent the reader off to supply a field that was
    /// never the problem.
    #[tokio::test]
    async fn an_unknown_action_on_a_known_tool_names_the_tool() {
        let r = dispatch(&cfg(), "spaces", &json!({ "action": "frobnicate" }))
            .await
            .expect_err("should be a malformed request");
        assert!(r.contains("frobnicate"), "{r}");
        assert!(r.contains("spaces"), "{r}");
    }

    /// `delete` keeps the disk unless told otherwise. The default being
    /// destructive is how a model's tidy-up loses someone's Windows install.
    #[test]
    fn delete_keeps_the_disk_by_default() {
        let args = json!({ "action": "delete", "name": "win-cad" });
        let keep = args
            .get("keep_disk")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        assert!(keep, "the default must be to keep the disk");

        let args = json!({ "action": "delete", "name": "win-cad", "keep_disk": false });
        let keep = args
            .get("keep_disk")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        assert!(!keep, "but it has to be losable when asked explicitly");
    }

    /// The create path reuses the REST struct, so the defaults cannot drift
    /// between the two ways in.
    #[test]
    fn create_defaults_match_the_rest_route() {
        let from_mcp: spaces::NewSpace =
            serde_json::from_value(json!({ "action": "create", "name": "win-cad" })).unwrap();
        assert_eq!(from_mcp.os, "win2k22");
        assert_eq!(from_mcp.vcpus, 8);
        assert_eq!(from_mcp.memory_mb, 16384);
        assert_eq!(from_mcp.disk_gb, 120);
        assert!(from_mcp.gpu.is_none());
    }

    /// The description has to carry the fact that decides the whole
    /// multi-agent story, because it is what the model reads before choosing.
    #[test]
    fn the_schema_explains_why_server_is_the_default() {
        let sp = spaces_tool();
        let os = sp["inputSchema"]["properties"]["os"]["description"]
            .as_str()
            .unwrap();
        assert!(os.contains("win2k22"));
        assert!(
            os.contains("concurrent"),
            "the model has to know Server is what licenses parallel sessions: {os}"
        );
    }
}
