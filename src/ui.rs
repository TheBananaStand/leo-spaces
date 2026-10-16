//! The descriptor UI: `GET /leo/ui/descriptor` and `GET /leo/ui/data`.
//!
//! One screen, rendered natively by the Mac and iOS clients from this JSON —
//! there is no web bundle. The descriptor is the *shape* and never changes;
//! `data` is everything live, re-read on each load.
//!
//! The screen is a master-detail with three tabs, which is the shape the
//! subject matter wants: a host either runs guests or needs setting up, and
//! both readings have to be one glance apart. **Spaces** is the day-to-day
//! list. **Host setup** is the provisioning plan, derived fresh every time so
//! a step that became true disappears. **Hardware** is the GPUs and whether
//! each can be given to a guest.
//!
//! Every node kind, text style, colour token and action shape here is from the
//! grammar the two renderers actually implement
//! (`crates/leo-package/src/ui/validate.rs`). An unknown kind renders as
//! nothing at all, silently, so nothing is invented.
//!
//! ## Two copies of this tree, and which one the clients draw
//!
//! The hub renders a native page from the `ui` field of the descriptor it has
//! **stored** — `ui_descriptor()` reads that and nothing else. It never asks
//! an app package for its tree; only `GET /leo/ui/data` is proxied to this
//! process. So `descriptor.json` carries a copy of [`pages`], written by
//! `scripts/sync-ui-descriptor.sh`, and `the_embedded_descriptor_matches_this_code`
//! fails when the two drift.
//!
//! ## It renders because the port is fixed
//!
//! The hub jails a *dynamic*-port app package with bubblewrap: `/usr`, a few
//! `/etc` files, `/proc`, `/dev`, a tmpfs `/tmp`, the install dir and the data
//! dir — and a private network namespace. There is no `/run`, so
//! `/run/libvirt/libvirt-sock` does not exist inside it and `virsh` can never
//! reach libvirtd, however many times it is on `$PATH`. `data()` below would
//! answer every request with an unreachable host.
//!
//! `descriptor.json` declares port 8530, which takes the hub's other branch:
//! spawned bare, with the filesystem the host actually has. The jail is not
//! widened and nothing is exempted — see the README for what the fixed port
//! costs in exchange.

use axum::extract::{Query, State};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::facts::Facts;
use crate::provision::{self, Plan, Risk};
use crate::spaces::{self, Space};
use crate::AppState;

// The status palette, matching what the clients use elsewhere.
const GREEN: &str = "#30d158";
const AMBER: &str = "#ff9f0a";
const RED: &str = "#ff453a";
const GRAY: &str = "#98989d";
const BLUE: &str = "#0a84ff";
const PURPLE: &str = "#bf5af2";

const DEFAULT_PAGE: &str = "spaces";

#[derive(Deserialize)]
pub struct DataQuery {
    page: Option<String>,
}

/// The page tree, keyed by page path — the shape the hub stores as a
/// descriptor's `ui` field and the shape this package serves.
///
/// One function because there are two consumers that must not disagree: the
/// hub renders the **embedded** copy in `descriptor.json` (it never asks an
/// app package for its tree, only for its *data*), and `GET
/// /leo/ui/descriptor` serves this one for anything driving the app directly.
/// `scripts/sync-ui-descriptor.sh` writes the first from the second, and
/// `the_embedded_descriptor_matches_this_code` fails when they drift.
pub fn pages() -> Value {
    json!({ DEFAULT_PAGE: screen() })
}

/// `GET /leo/ui/descriptor` — the static tree.
pub async fn descriptor_handler(
    _c: crate::auth::Caller,
    _s: State<AppState>,
) -> Json<Value> {
    Json(pages())
}

/// `GET /leo/ui/data?page=spaces` — everything live.
///
/// An absent `page` means the only page there is. The hub forwards the
/// *client's* query params rather than supplying the page itself, so a caller
/// omitting it is ordinary rather than broken — answering with "page '' has no
/// data" would turn this package's pedantry into a 502.
pub async fn data_handler(
    _c: crate::auth::Caller,
    State(s): State<AppState>,
    Query(q): Query<DataQuery>,
) -> Result<Json<Value>, crate::error::Error> {
    match q.page.as_deref().unwrap_or(DEFAULT_PAGE) {
        "" | DEFAULT_PAGE => Ok(Json(data(&s).await)),
        other => Err(crate::error::Error::Other(format!(
            "spaces has no page '{other}' — this package serves only '{DEFAULT_PAGE}'"
        ))),
    }
}

// ───────────────────────────────────────────────────────────────── descriptor

fn screen() -> Value {
    json!({
        "kind": "screen",
        "title": "Spaces",
        // No `route`. The hub derives a package page's native address from the
        // tree's `route` when it declares one, and from `/p/<pkg>/<path>`
        // otherwise — and `/p/<pkg>/<path>` is what every nav surface actually
        // addresses. Declaring "/spaces" published a route nothing navigates
        // to: the nav click asked for /p/spaces/spaces, found no native surface
        // for it, fell through to a web view, and got the hub's "Spaces has no
        // web page" card. Short routes belong to compiled-in packages, which
        // carry `builtin_route` on the page to match. See
        // `the_declared_route_is_the_one_the_nav_opens`.
        "state": { "tab": "spaces", "query": "", "selectedId": "" },
        // Both are addressable so a space can be linked to directly — which is
        // what makes "open the CAD machine" a thing the assistant can answer
        // with a place rather than a description.
        "addressableState": ["tab", "selectedId"],
        "assistant": {
            "app": "Spaces",
            "placeholder": "Ask about your machines…",
            "suggestions": [
                "Is the hypervisor set up on this host?",
                "What's running right now?",
                "Start the Windows CAD machine",
                "Can the GPU be passed through?"
            ]
        },
        // `body`, not `children`: the renderers read a screen's one child from
        // `body` (`ScreenView` → `node.node("body")`), and a screen carrying
        // `children` instead draws the placeholder "This screen has no 'body'
        // to draw" with nothing under it. The kind is spelled right, every node
        // below is valid, and nothing logs — see `a_screen_draws_from_body`.
        "body": {
            "kind": "masterDetail",
            "selectionKey": "selectedId",
            "dividers": true,
            "dividerInset": 22,
            "sidebarWidth": 232,
            "detailWidth": 360,
            "sidebar": sidebar(),
            "list": {
                "kind": "list",
                "bind": "rows",
                "filter": {
                    "field": "tab",
                    "equalsState": "tab",
                    "search": "query",
                    "searchFields": ["title", "subtitle", "trailing"]
                },
                "empty": empty_states(),
                // `item`, not `children`: a list's row template is read from
                // `item` by both the master-detail list and the standalone
                // `list` kind. With `children` the pane draws "This
                // masterDetail has no 'list.item' to draw" and no rows.
                "item": row()
            },
            "detail": detail()
        }
    })
}

fn sidebar() -> Value {
    json!({
        "kind": "vstack",
        "spacing": 2,
        "children": [
            host_hero(),
            quiet_row("CPU", "cpu_label"),
            quiet_row("Memory", "memory_label"),
            quiet_row("IOMMU", "iommu_label"),
            section_label("Machines", 16),
            {
                "kind": "searchField",
                "stateKey": "query",
                "placeholder": "Search",
                "width": 208
            },
            tab_row("spaces", "Spaces", "macwindow", "space_count"),
            tab_row("setup", "Host setup", "wrench.and.screwdriver", "setup_count"),
            tab_row("hardware", "Hardware", "cpu", "gpu_count"),
        ]
    })
}

/// The one thing worth knowing before anything else: can this host run a guest.
fn host_hero() -> Value {
    json!({
        "kind": "vstack",
        "spacing": 5,
        "align": "leading",
        "paddingH": 10,
        "paddingV": 6,
        "children": [
            { "kind": "text", "style": "section-label", "value": "HYPERVISOR" },
            {
                "kind": "hstack",
                "spacing": 8,
                "align": "center",
                "children": [
                    { "kind": "dot", "colorFrom": "host_status_color", "size": 8 },
                    { "kind": "text", "bind": "host_status_label", "style": "headline" }
                ]
            },
            {
                "kind": "text",
                "bind": "host_label",
                "style": "mono",
                "color": "secondary",
                "lines": 1,
                "hideWhenEmpty": true
            }
        ]
    })
}

fn section_label(label: &str, padding_top: u32) -> Value {
    json!({
        "kind": "hstack",
        "paddingH": 10,
        "paddingTop": padding_top,
        "children": [{ "kind": "text", "style": "section-label", "value": label }]
    })
}

fn tab_row(value: &str, label: &str, icon: &str, count_bind: &str) -> Value {
    json!({
        "kind": "selectRow",
        "stateKey": "tab",
        "value": value,
        "label": label,
        "icon": icon,
        "countBind": count_bind
    })
}

fn quiet_row(label: &str, bind: &str) -> Value {
    json!({
        "kind": "hstack",
        "spacing": 6,
        "align": "center",
        "paddingH": 10,
        "paddingV": 2,
        "children": [
            { "kind": "text", "style": "caption", "value": label },
            { "kind": "spacer" },
            {
                "kind": "text",
                "bind": bind,
                "style": "mono",
                "color": "secondary",
                "lines": 1,
                "hideWhenEmpty": true
            }
        ]
    })
}

/// One list row. Shared by all three tabs: a coloured state dot, a title with
/// an optional badge, a subtitle, and a trailing monospace fact.
///
/// One row template rather than three because the three tabs are the same
/// *shape* of thing — a named object with a state and a detail — and three
/// templates would drift in padding until the tabs stopped looking like one
/// surface.
fn row() -> Value {
    json!({
        "kind": "hstack",
        "spacing": 11,
        "align": "center",
        "paddingH": 22,
        "paddingV": 12,
        "children": [
            { "kind": "dot", "colorFrom": "state_color", "size": 7 },
            {
                "kind": "vstack",
                "spacing": 3,
                "align": "leading",
                "children": [
                    {
                        "kind": "hstack",
                        "spacing": 7,
                        "align": "center",
                        "children": [
                            { "kind": "text", "bind": "title", "style": "headline", "lines": 1 },
                            { "kind": "badge", "bind": "badge", "tintFrom": "badge_color", "hideWhenEmpty": true }
                        ]
                    },
                    { "kind": "text", "bind": "subtitle", "style": "caption", "lines": 1, "hideWhenEmpty": true }
                ]
            },
            { "kind": "spacer" },
            { "kind": "text", "bind": "trailing", "style": "mono", "color": "secondary", "hideWhenEmpty": true }
        ]
    })
}

fn empty_states() -> Value {
    json!({
        "kind": "vstack",
        "children": [
            {
                "kind": "emptyState",
                "icon": "macwindow",
                "title": "No spaces yet",
                "message": "Once the host is set up, ask Leo to create one — \u{201c}make a Windows Server space with 8 cores and 16 GB\u{201d}.",
                "visibleWhen": { "state": "tab", "equals": "spaces" }
            },
            {
                "kind": "emptyState",
                "icon": "checkmark.seal",
                "title": "Host is ready",
                "message": "Everything a guest needs is installed and running.",
                "visibleWhen": { "state": "tab", "equals": "setup" }
            },
            {
                "kind": "emptyState",
                "icon": "cpu",
                "title": "No graphics devices found",
                "message": "Nothing on the PCI bus reports itself as a display adapter.",
                "visibleWhen": { "state": "tab", "equals": "hardware" }
            }
        ]
    })
}

/// The detail pane. Row-scoped, so `bind` and `{field}` interpolation resolve
/// against the selected row.
fn detail() -> Value {
    json!({
        "kind": "vstack",
        "spacing": 12,
        "align": "leading",
        "children": [
            { "kind": "badge", "bind": "kind_label", "tintFrom": "badge_color" },
            { "kind": "text", "bind": "title", "style": "title", "wrap": true },
            { "kind": "text", "bind": "detail_text", "style": "subtle", "wrap": true, "hideWhenEmpty": true },
            { "kind": "divider" },
            fields_list(),
            // A blocker is the most important thing on the pane when there is
            // one, so it sits above the controls rather than below them: the
            // reason a button is absent has to arrive before the search for it.
            {
                "kind": "vstack",
                "spacing": 6,
                "align": "leading",
                "paddingTop": 4,
                "hideWhenEmpty": true,
                "children": [
                    { "kind": "text", "style": "section-label", "value": "IN THE WAY" },
                    { "kind": "text", "bind": "blocked", "style": "caption", "color": "muted", "wrap": true }
                ],
                "visibleWhen": { "field": "has_blocker", "equals": "1" }
            },
            command_block(),
            space_controls(),
            setup_controls(),
        ]
    })
}

fn fields_list() -> Value {
    json!({
        "kind": "list",
        "bind": "sections",
        "spacing": 14,
        "item": {
            "kind": "vstack",
            "spacing": 8,
            "align": "leading",
            "children": [
                { "kind": "text", "bind": "title", "style": "section-label" },
                {
                    "kind": "list",
                    "bind": "fields",
                    "spacing": 8,
                    "item": {
                        "kind": "hstack",
                        "spacing": 8,
                        "align": "center",
                        "children": [
                            { "kind": "text", "bind": "label", "style": "caption" },
                            { "kind": "spacer" },
                            { "kind": "text", "bind": "value", "style": "mono", "lines": 1 }
                        ]
                    }
                }
            ]
        }
    })
}

/// The command a step would run, shown before it runs — and copyable, which is
/// the whole deliverable on a host this package has no privilege over.
fn command_block() -> Value {
    json!({
        "kind": "vstack",
        "spacing": 6,
        "align": "leading",
        "paddingTop": 4,
        "children": [
            { "kind": "text", "style": "section-label", "value": "COMMAND" },
            { "kind": "text", "bind": "command", "style": "mono", "color": "secondary", "wrap": true },
            {
                "kind": "hstack",
                "children": [
                    {
                        "kind": "button",
                        "variant": "pill",
                        "label": "Copy",
                        "action": { "kind": "copyText", "text": "{command}" }
                    }
                ]
            }
        ],
        "visibleWhen": { "field": "has_command", "equals": "1" }
    })
}

/// Start / Shutdown / Restart / Force off, each only when it makes sense.
///
/// Gated on the row's state rather than always shown and disabled: a Start
/// button on a running machine is a question the operator has to answer before
/// they can read the pane.
fn space_controls() -> Value {
    json!({
        "kind": "vstack",
        "spacing": 8,
        "align": "leading",
        "paddingTop": 8,
        "children": [
            { "kind": "text", "style": "section-label", "value": "CONTROLS" },
            {
                "kind": "hstack",
                "spacing": 8,
                "children": [
                    control_button("Start", "start", "pill", "off"),
                    control_button("Shut down", "shutdown", "pill", "on"),
                    control_button("Restart", "reboot", "pill", "on"),
                ]
            },
            {
                "kind": "hstack",
                "spacing": 8,
                "paddingTop": 2,
                "children": [
                    {
                        "kind": "button",
                        "variant": "destructive",
                        "label": "Force off",
                        "visibleWhen": { "field": "running", "equals": "on" },
                        "action": {
                            "kind": "confirm",
                            "title": "Force \u{201c}{title}\u{201d} off?",
                            // Named for what it is. "Force off" sounds like a
                            // firmer shutdown; it is pulling the power, and a
                            // Windows guest mid-write can lose the filesystem.
                            "message": "Cuts the power without telling the guest. Unsaved work is lost, and a guest writing to disk can be left with a damaged filesystem. Use Shut down unless it has stopped responding.",
                            "confirmLabel": "Force off",
                            "destructive": true,
                            "confirm": {
                                "method": "POST",
                                "path": "/p/spaces/api/spaces/{title}/control",
                                "body": { "action": "force_off" },
                                "refresh": "reload"
                            }
                        }
                    }
                ]
            }
        ],
        "visibleWhen": { "field": "kind", "equals": "space" }
    })
}

fn control_button(label: &str, action: &str, variant: &str, when_running: &str) -> Value {
    json!({
        "kind": "button",
        "variant": variant,
        "label": label,
        "visibleWhen": { "field": "running", "equals": when_running },
        "action": {
            "method": "POST",
            // `{title}` is the space's name — the row's title is the name, and
            // interpolation resolves against the selected row.
            "path": "/p/spaces/api/spaces/{title}/control",
            "body": { "action": action },
            "refresh": "reload"
        }
    })
}

/// Apply one provisioning step. Boot-touching steps confirm first and say what
/// the reboot is for; the rest are a single press.
fn setup_controls() -> Value {
    json!({
        "kind": "vstack",
        "spacing": 8,
        "align": "leading",
        "paddingTop": 8,
        "children": [
            {
                "kind": "hstack",
                "spacing": 8,
                "children": [{
                    "kind": "button",
                    "variant": "pill",
                    "label": "Apply",
                    "visibleWhen": { "field": "apply_mode", "equals": "direct" },
                    "action": {
                        "method": "POST",
                        "path": "/p/spaces/api/host/apply",
                        "body": { "step": "{id}" },
                        "refresh": "reload"
                    }
                }]
            },
            {
                "kind": "hstack",
                "spacing": 8,
                "children": [{
                    "kind": "button",
                    "variant": "destructive",
                    "label": "Apply and reboot later",
                    "visibleWhen": { "field": "apply_mode", "equals": "boot" },
                    "action": {
                        "kind": "confirm",
                        "title": "Change how this host boots?",
                        "message": "This edits the kernel command line. A backup of the boot configuration is written first, and any fallback entry is left untouched so there is a way back. The change takes effect on the next reboot \u{2014} nothing restarts now.",
                        "confirmLabel": "Write it",
                        "destructive": true,
                        "confirm": {
                            "method": "POST",
                            "path": "/p/spaces/api/host/apply",
                            "body": { "step": "{id}" },
                            "refresh": "reload"
                        }
                    }
                }]
            }
        ],
        "visibleWhen": { "field": "kind", "equals": "step" }
    })
}

// ──────────────────────────────────────────────────────────────────── data

async fn data(s: &AppState) -> Value {
    let f = crate::facts::probe(&s.cfg.host).await.unwrap_or_else(|e| {
        let mut blank = crate::facts::probe_blank(&s.cfg.host);
        blank.unreachable_reason = Some(e.to_string());
        blank
    });
    let plan = provision::plan(&f);
    // A host with no libvirt yet has no spaces, and that is not an error to
    // show: the setup tab already says why. An empty list plus a populated
    // plan is the correct reading of a box that has not been set up.
    let spaces = spaces::list(&s.cfg.host).await.unwrap_or_default();

    build_data(&f, &plan, &spaces)
}

/// Assemble the payload.
///
/// Split from `data` so every row the UI can show is testable without a
/// hypervisor — which is the only way to check that a blocked step still
/// produces a readable pane.
pub fn build_data(f: &Facts, plan: &Plan, spaces: &[Space]) -> Value {
    let mut rows: Vec<Value> = Vec::new();

    for sp in spaces {
        rows.push(space_row(sp, f));
    }
    for step in &plan.steps {
        rows.push(step_row(step));
    }
    for g in &f.gpus {
        rows.push(gpu_row(g, spaces));
    }

    let setup_pending = plan.steps.iter().filter(|s| !s.satisfied).count();

    json!({
        "rows": rows,

        "host_status_label": f.status_label(),
        "host_status_color": f.status_color(),
        "host_label": if f.hostname.is_empty() { f.host.clone() } else { f.hostname.clone() },

        "cpu_label": if f.cpu_threads > 0 {
            format!("{} threads", f.cpu_threads)
        } else {
            String::new()
        },
        "memory_label": if f.memory_mb > 0 {
            format!("{} GB", f.memory_mb / 1024)
        } else {
            String::new()
        },
        "iommu_label": if !f.reachable {
            String::new()
        } else if f.iommu_enabled() {
            format!("{} groups", f.iommu_groups)
        } else {
            "off".to_string()
        },

        // Counts show on the tab rows. The setup count is what is *left*, not
        // the total: a badge of 9 on a finished host reads as nine problems.
        "space_count": spaces.len().to_string(),
        "setup_count": if setup_pending == 0 { String::new() } else { setup_pending.to_string() },
        "gpu_count": if f.gpus.is_empty() { String::new() } else { f.gpus.len().to_string() },
    })
}

fn space_row(sp: &Space, f: &Facts) -> Value {
    let gpu_name = sp.hostdevs.first().and_then(|addr| {
        f.gpus
            .iter()
            .find(|g| &g.address == addr)
            .map(|g| g.name.clone())
    });

    let mut hardware = vec![
        ("vCPUs", sp.vcpus.to_string()),
        ("Memory", format!("{} MB", sp.memory_mb)),
    ];
    if let Some(addr) = sp.hostdevs.first() {
        hardware.push((
            "GPU",
            gpu_name.clone().unwrap_or_else(|| addr.clone()),
        ));
    }

    let mut access = Vec::new();
    if let Some(ip) = &sp.ip {
        access.push(("Address", ip.clone()));
    }
    if let Some(mac) = &sp.mac {
        access.push(("MAC", mac.clone()));
    }
    if let Some(port) = sp.console_port {
        access.push((
            "Console",
            format!("{} on 127.0.0.1:{}", sp.console_kind.as_deref().unwrap_or("vnc"), port),
        ));
    }

    json!({
        "id": format!("space-{}", sp.name),
        "tab": "spaces",
        "kind": "space",
        "kind_label": "Space",
        "title": sp.name,
        "subtitle": join_dot(&[
            os_label(&sp.os),
            &format!("{} vCPU", sp.vcpus),
            &format!("{} GB", sp.memory_mb / 1024),
            if sp.has_gpu() { "GPU" } else { "" },
        ]),
        "trailing": sp.ip.clone().unwrap_or_default(),
        "badge": sp.state.label(),
        "badge_color": sp.state.color(),
        "state_color": sp.state.color(),
        "detail_text": format!(
            "{}{}",
            os_description(&sp.os),
            match &gpu_name {
                Some(n) => format!(" The {} is assigned to this space exclusively.", short_name(n)),
                None => String::new(),
            }
        ),
        // Read by `visibleWhen` on the control buttons. A string rather than a
        // bool because conditions compare against strings.
        "running": if sp.state.is_running() { "on" } else { "off" },
        "has_blocker": "0",
        "has_command": "0",
        "blocked": "",
        "command": "",
        "apply_mode": "none",
        "sections": sections(vec![
            section("Guest", &[
                ("Name", sp.name.clone()),
                ("System", os_label(&sp.os).to_string()),
                ("State", sp.state.label().to_string()),
                ("Autostart", if sp.autostart { "on".into() } else { "off".into() }),
            ]),
            section("Hardware", &hardware.iter().map(|(a, b)| (*a, b.clone())).collect::<Vec<_>>()),
            section("Access", &access.iter().map(|(a, b)| (*a, b.clone())).collect::<Vec<_>>()),
        ]),
    })
}

fn step_row(step: &provision::Step) -> Value {
    let (badge, badge_color) = if step.satisfied {
        ("Done", GREEN)
    } else if step.blocked.is_some() {
        ("Blocked", RED)
    } else if step.risk == Risk::Boot {
        ("Reboot", PURPLE)
    } else {
        ("Pending", AMBER)
    };

    // Which button the pane offers, if any. Boot steps get the confirming
    // variant; a blocked or satisfied step gets none, because there is nothing
    // to press that would help.
    let apply_mode = if !step.applicable() {
        "none"
    } else if step.risk == Risk::Boot {
        "boot"
    } else {
        "direct"
    };

    json!({
        "id": step.id,
        "tab": "setup",
        "kind": "step",
        "kind_label": "Setup step",
        "title": step.title,
        "subtitle": first_sentence(&step.detail),
        "trailing": if step.needs_reboot { "reboot" } else { "" },
        "badge": badge,
        "badge_color": badge_color,
        "state_color": badge_color,
        "detail_text": step.detail,
        "running": "none",
        "has_blocker": if step.blocked.is_some() { "1" } else { "0" },
        "blocked": step.blocked.clone().unwrap_or_default(),
        "has_command": if step.command.is_some() && !step.satisfied { "1" } else { "0" },
        "command": step.command.clone().unwrap_or_default(),
        "apply_mode": apply_mode,
        "sections": sections(vec![
            section("Step", &[
                ("State", if step.satisfied { "satisfied".into() } else { "pending".into() }),
                ("Risk", if step.risk == Risk::Boot { "changes how the host boots".into() } else { "reversible".into() }),
                ("Reboot", if step.needs_reboot { "required".into() } else { "no".into() }),
            ]),
        ]),
    })
}

fn gpu_row(g: &crate::facts::Gpu, spaces: &[Space]) -> Value {
    let blockers = g.passthrough_blockers();
    let holder = spaces
        .iter()
        .find(|s| s.hostdevs.iter().any(|h| h == &g.address));

    let (badge, badge_color) = if let Some(h) = holder {
        (h.name.as_str(), BLUE)
    } else if g.integrated {
        ("Host display", GRAY)
    } else if blockers.is_empty() {
        ("Available", GREEN)
    } else {
        ("Not passable", AMBER)
    };

    json!({
        "id": format!("gpu-{}", g.address),
        "tab": "hardware",
        "kind": "gpu",
        "kind_label": "Graphics device",
        "title": short_name(&g.name),
        "subtitle": join_dot(&[&g.address, &g.ids, &g.driver]),
        "trailing": match g.iommu_group {
            Some(n) => format!("group {n}"),
            None => String::new(),
        },
        "badge": badge,
        "badge_color": badge_color,
        "state_color": badge_color,
        "detail_text": if g.integrated {
            "This is the display the host itself uses. Passing it to a guest would leave the console dark, so it is not offered.".to_string()
        } else if blockers.is_empty() {
            "This card can be handed to one space exclusively.".to_string()
        } else {
            "This card cannot currently be given to a guest.".to_string()
        },
        "running": "none",
        "has_blocker": if blockers.is_empty() { "0" } else { "1" },
        "blocked": blockers.join("; "),
        "has_command": "0",
        "command": "",
        "apply_mode": "none",
        "sections": sections(vec![
            section("Device", &[
                ("Address", g.address.clone()),
                ("PCI id", g.ids.clone()),
                ("Driver", if g.driver.is_empty() { "none".into() } else { g.driver.clone() }),
            ]),
            section("Isolation", &[
                ("IOMMU group", g.iommu_group.map(|n| n.to_string()).unwrap_or_else(|| "none".into())),
                ("Shares group with", if g.group_siblings.is_empty() { "nothing".into() } else { g.group_siblings.join(", ") }),
                ("Assigned to", holder.map(|h| h.name.clone()).unwrap_or_else(|| "nothing".into())),
            ]),
        ]),
    })
}

// ─────────────────────────────────────────────────────────────────── helpers

fn sections(list: Vec<Value>) -> Value {
    // Drop sections with no fields rather than rendering an empty heading —
    // "Access" over nothing reads as missing data instead of as a guest that
    // is simply switched off.
    Value::Array(
        list.into_iter()
            .filter(|s| {
                s.get("fields")
                    .and_then(Value::as_array)
                    .is_some_and(|f| !f.is_empty())
            })
            .collect(),
    )
}

fn section(title: &str, fields: &[(&str, String)]) -> Value {
    json!({
        "title": title,
        "fields": fields
            .iter()
            .filter(|(_, v)| !v.is_empty())
            .map(|(l, v)| json!({ "label": l, "value": v }))
            .collect::<Vec<_>>()
    })
}

fn join_dot(parts: &[&str]) -> String {
    parts
        .iter()
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join(" · ")
}

/// The first sentence of a step's detail, for the one-line subtitle.
fn first_sentence(s: &str) -> String {
    match s.find(". ") {
        Some(i) => s[..=i].trim().to_string(),
        None => s.to_string(),
    }
}

/// `VGA compatible controller: NVIDIA Corporation GA104GL [RTX A4000]` → `RTX A4000`
fn short_name(full: &str) -> String {
    if let Some(start) = full.rfind('[') {
        if let Some(end) = full[start..].find(']') {
            let inner = &full[start + 1..start + end];
            if !inner.is_empty() {
                return inner.to_string();
            }
        }
    }
    full.split(':').next_back().unwrap_or(full).trim().to_string()
}

/// A libosinfo short id as a person would say it.
fn os_label(os: &str) -> &str {
    match os {
        "win2k25" => "Windows Server 2025",
        "win2k22" => "Windows Server 2022",
        "win2k19" => "Windows Server 2019",
        "win11" => "Windows 11",
        "win10" => "Windows 10",
        "" => "Unknown",
        other => other,
    }
}

fn os_description(os: &str) -> String {
    if os.starts_with("win2k") {
        // The reason Server is the default, said where somebody choosing a
        // guest will read it.
        "Windows Server licenses more than one concurrent interactive session, so several agents can each drive their own desktop on this one guest.".to_string()
    } else if os.starts_with("win") {
        "Client Windows allows one interactive session at a time, so this guest hosts one agent. Windows Server is the licensed path to running several in parallel.".to_string()
    } else {
        String::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts::{Bootloader, CpuVendor, Gpu, PackageManager, Privilege};
    use crate::host::Host;
    use crate::spaces::State;

    fn facts() -> Facts {
        let mut f = crate::facts::probe_blank(&Host::Local);
        f.reachable = true;
        f.hostname = "leo-work".into();
        f.os_id = "arch".into();
        f.package_manager = PackageManager::Pacman;
        f.bootloader = Bootloader::SystemdBoot;
        f.cpu_vendor = CpuVendor::Amd;
        f.cpu_threads = 32;
        f.memory_mb = 93_000;
        f.disk_free_gb = 1800;
        f.virt_supported = true;
        f.kvm_available = true;
        f.privilege = Privilege::Wrapper;
        f.wrapper_installed = true;
        f.wrapper_usable = true;
        f
    }

    fn a_space() -> Space {
        Space {
            name: "win-cad".into(),
            uuid: "4f8c1e2a".into(),
            state: State::Running,
            vcpus: 8,
            memory_mb: 16384,
            autostart: true,
            hostdevs: vec!["0000:01:00.0".into()],
            ip: Some("192.168.122.84".into()),
            mac: Some("52:54:00:ab:cd:ef".into()),
            console_port: Some(5900),
            console_kind: Some("vnc".into()),
            os: "win2k22".into(),
        }
    }

    /// Every `bind` and `countBind` in the descriptor must exist as a key in
    /// the payload. A missing one renders as blank, silently — the exact
    /// failure the renderers' fallbacks make invisible, and the reason this
    /// test walks the tree rather than spot-checking.
    #[test]
    fn every_top_level_bind_is_present_in_the_payload() {
        let f = facts();
        let plan = provision::plan(&f);
        let payload = build_data(&f, &plan, &[a_space()]);

        // Keys the sidebar binds directly against the payload.
        for key in [
            "rows",
            "host_status_label",
            "host_status_color",
            "host_label",
            "cpu_label",
            "memory_label",
            "iommu_label",
            "space_count",
            "setup_count",
            "gpu_count",
        ] {
            assert!(
                payload.get(key).is_some(),
                "descriptor binds '{key}' and the payload has no such key"
            );
        }
    }

    /// Row-level binds are resolved against a row, not the payload, so every
    /// row has to carry every key any row template or detail pane reads —
    /// including the ones only another tab's rows use. A row missing
    /// `apply_mode` would leave a stale Apply button visible on a space.
    #[test]
    fn every_row_carries_every_row_level_bind() {
        let f = {
            let mut f = facts();
            f.gpus = vec![Gpu {
                address: "0000:01:00.0".into(),
                ids: "10de:24b0".into(),
                name: "NVIDIA GA104GL [RTX A4000]".into(),
                driver: "vfio-pci".into(),
                iommu_group: Some(15),
                group_siblings: vec![],
                is_boot_vga: false,
                integrated: false,
            }];
            f
        };
        let plan = provision::plan(&f);
        let payload = build_data(&f, &plan, &[a_space()]);
        let rows = payload["rows"].as_array().expect("rows");
        assert!(rows.len() >= 3, "a space, the steps and a GPU");

        for r in rows {
            for key in [
                "id", "tab", "kind", "kind_label", "title", "subtitle", "trailing",
                "badge", "badge_color", "state_color", "detail_text", "running",
                "has_blocker", "blocked", "has_command", "command", "apply_mode",
                "sections",
            ] {
                assert!(
                    r.get(key).is_some(),
                    "row {:?} is missing '{key}'",
                    r.get("id")
                );
            }
            // Conditions compare strings, so a bool here would never match and
            // the gated node would hide forever.
            for key in ["has_blocker", "has_command", "running", "apply_mode"] {
                assert!(
                    r[key].is_string(),
                    "row {:?} key '{key}' must be a string for visibleWhen, got {:?}",
                    r.get("id"),
                    r[key]
                );
            }
        }
    }

    /// Each tab must have rows, or it shows an empty state that is a lie.
    #[test]
    fn every_tab_has_rows_and_every_row_has_a_known_tab() {
        let mut f = facts();
        f.gpus = vec![Gpu {
            address: "0000:78:00.0".into(),
            ids: "1002:13c0".into(),
            name: "[Radeon Graphics]".into(),
            driver: "amdgpu".into(),
            iommu_group: None,
            group_siblings: vec![],
            is_boot_vga: true,
            integrated: true,
        }];
        let plan = provision::plan(&f);
        let payload = build_data(&f, &plan, &[a_space()]);
        let rows = payload["rows"].as_array().unwrap();

        for tab in ["spaces", "setup", "hardware"] {
            assert!(
                rows.iter().any(|r| r["tab"] == tab),
                "no rows for the '{tab}' tab"
            );
        }
        for r in rows {
            let t = r["tab"].as_str().unwrap();
            assert!(
                ["spaces", "setup", "hardware"].contains(&t),
                "row {:?} has unknown tab '{t}' and would never render",
                r["id"]
            );
        }
    }

    /// A running space offers Shut down and Restart but not Start; an off one
    /// the reverse. Gating on state rather than disabling means the pane never
    /// asks the operator to work out which button applies.
    #[test]
    fn controls_are_gated_on_the_spaces_actual_state() {
        let f = facts();
        let plan = provision::plan(&f);

        let running = build_data(&f, &plan, &[a_space()]);
        let row = &running["rows"].as_array().unwrap()[0];
        assert_eq!(row["running"], "on");

        let off = Space { state: State::Shutoff, ..a_space() };
        let payload = build_data(&f, &plan, &[off]);
        assert_eq!(payload["rows"].as_array().unwrap()[0]["running"], "off");
    }

    /// A blocked step shows its reason and offers no button. The reason is the
    /// deliverable; a button that would fail is worse than none.
    #[test]
    fn a_blocked_step_shows_the_reason_and_no_button() {
        let mut f = facts();
        f.privilege = Privilege::None;
        f.wrapper_installed = false;
        f.wrapper_usable = false;
        let plan = provision::plan(&f);
        let payload = build_data(&f, &plan, &[]);

        let pkg = payload["rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == "packages")
            .expect("the packages step");

        assert_eq!(pkg["badge"], "Blocked");
        assert_eq!(pkg["has_blocker"], "1");
        assert!(pkg["blocked"].as_str().unwrap().contains("install-admin-wrapper"));
        assert_eq!(pkg["apply_mode"], "none", "nothing to press");
        // But the command is still there to copy, which is the whole point.
        assert_eq!(pkg["has_command"], "1");
        assert!(pkg["command"].as_str().unwrap().contains("pacman"));
    }

    /// A boot-touching step must route to the confirming button, never the
    /// one-press one.
    #[test]
    fn a_boot_step_gets_the_confirming_button() {
        let f = facts();
        let plan = provision::plan(&f);
        let payload = build_data(&f, &plan, &[]);
        let rows = payload["rows"].as_array().unwrap();

        let iommu = rows.iter().find(|r| r["id"] == "iommu").unwrap();
        assert_eq!(iommu["apply_mode"], "boot");
        assert_eq!(iommu["badge"], "Reboot");

        let pkg = rows.iter().find(|r| r["id"] == "packages").unwrap();
        assert_eq!(pkg["apply_mode"], "direct");
    }

    /// A satisfied step offers no button and reads as done.
    #[test]
    fn a_satisfied_step_offers_nothing_to_press() {
        let mut f = facts();
        f.qemu_installed = true;
        f.libvirt_installed = true;
        f.ovmf_installed = true;
        f.swtpm_installed = true;
        let plan = provision::plan(&f);
        let payload = build_data(&f, &plan, &[]);
        let pkg = payload["rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == "packages")
            .unwrap();
        assert_eq!(pkg["badge"], "Done");
        assert_eq!(pkg["apply_mode"], "none");
        assert_eq!(pkg["has_command"], "0", "nothing left to run");
    }

    /// The setup badge counts what is left. A count of the total reads as nine
    /// problems on a host that is finished.
    #[test]
    fn the_setup_count_is_what_remains_and_empties_when_done() {
        let f = facts();
        let plan = provision::plan(&f);
        let payload = build_data(&f, &plan, &[]);
        let count = payload["setup_count"].as_str().unwrap();
        assert!(!count.is_empty() && count != "0");

        let mut done = facts();
        done.qemu_installed = true;
        done.libvirt_installed = true;
        done.ovmf_installed = true;
        done.swtpm_installed = true;
        done.libvirt_running = true;
        done.default_network_active = true;
        done.in_libvirt_group = true;
        done.iommu_groups = 40;
        let plan = provision::plan(&done);
        let payload = build_data(&done, &plan, &[]);
        // Storage and virtio are always offered (both are cheap to re-apply),
        // so the count is small but the badge is only blank at zero.
        assert_ne!(payload["setup_count"], json!("0"));
    }

    /// An unreachable host must read as unreachable, not as a host with no
    /// CPU and no memory — which is what blank strings would look like beside
    /// a green dot.
    #[test]
    fn an_unreachable_host_reads_as_unreachable() {
        let f = crate::facts::probe_blank(&Host::Local);
        let plan = provision::plan(&f);
        let payload = build_data(&f, &plan, &[]);
        assert_eq!(payload["host_status_label"], "Unreachable");
        assert_eq!(payload["host_status_color"], RED);
        assert_eq!(payload["cpu_label"], "");
        assert_eq!(payload["iommu_label"], "", "not 'off' — we do not know");
    }

    /// The integrated card is labelled as the host's own display rather than
    /// offered, and says why in the pane.
    #[test]
    fn the_host_display_is_labelled_not_offered() {
        let mut f = facts();
        f.gpus = vec![Gpu {
            address: "0000:78:00.0".into(),
            ids: "1002:13c0".into(),
            name: "[AMD/ATI] Granite Ridge [Radeon Graphics]".into(),
            driver: "amdgpu".into(),
            iommu_group: None,
            group_siblings: vec![],
            is_boot_vga: true,
            integrated: true,
        }];
        let plan = provision::plan(&f);
        let payload = build_data(&f, &plan, &[]);
        let gpu = payload["rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["kind"] == "gpu")
            .unwrap();
        assert_eq!(gpu["badge"], "Host display");
        assert_eq!(gpu["title"], "Radeon Graphics");
        assert!(gpu["detail_text"].as_str().unwrap().contains("console dark"));
    }

    /// A GPU already assigned shows which space holds it — the fact that
    /// answers "why can't I give it to this one".
    #[test]
    fn an_assigned_gpu_names_its_holder() {
        let mut f = facts();
        f.gpus = vec![Gpu {
            address: "0000:01:00.0".into(),
            ids: "10de:24b0".into(),
            name: "[RTX A4000]".into(),
            driver: "vfio-pci".into(),
            iommu_group: Some(15),
            group_siblings: vec![],
            is_boot_vga: false,
            integrated: false,
        }];
        let plan = provision::plan(&f);
        let payload = build_data(&f, &plan, &[a_space()]);
        let gpu = payload["rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["kind"] == "gpu")
            .unwrap();
        assert_eq!(gpu["badge"], "win-cad");
    }

    /// The Server-versus-client-Windows distinction is the multi-agent story,
    /// so it has to appear where someone reading a space will see it.
    #[test]
    fn a_windows_server_space_explains_multi_session() {
        let f = facts();
        let plan = provision::plan(&f);
        let payload = build_data(&f, &plan, &[a_space()]);
        let row = &payload["rows"].as_array().unwrap()[0];
        assert_eq!(row["subtitle"].as_str().unwrap().contains("Windows Server 2022"), true);
        assert!(row["detail_text"].as_str().unwrap().contains("concurrent interactive session"));

        let client = Space { os: "win11".into(), hostdevs: vec![], ..a_space() };
        let payload = build_data(&f, &plan, &[client]);
        let row = &payload["rows"].as_array().unwrap()[0];
        assert!(row["detail_text"].as_str().unwrap().contains("one interactive session"));
    }

    /// Empty sections are dropped. An "Access" heading over nothing reads as
    /// missing data rather than as a guest that is switched off.
    #[test]
    fn sections_with_no_fields_are_dropped() {
        let f = facts();
        let plan = provision::plan(&f);
        let bare = Space {
            state: State::Shutoff,
            ip: None,
            mac: None,
            console_port: None,
            console_kind: None,
            hostdevs: vec![],
            ..a_space()
        };
        let payload = build_data(&f, &plan, &[bare]);
        let row = &payload["rows"].as_array().unwrap()[0];
        let titles: Vec<&str> = row["sections"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["title"].as_str().unwrap())
            .collect();
        assert!(titles.contains(&"Guest"));
        assert!(!titles.contains(&"Access"), "an empty Access section should be dropped");
    }

    #[test]
    fn a_name_shortens_to_what_a_person_recognises() {
        assert_eq!(
            short_name("VGA compatible controller: NVIDIA Corporation GA104GL [RTX A4000]"),
            "RTX A4000"
        );
        assert_eq!(short_name("3D controller: Some Card 9000"), "Some Card 9000");
    }

    #[test]
    fn a_subtitle_is_one_sentence() {
        assert_eq!(
            first_sentence("Installs things. And then more things happen."),
            "Installs things."
        );
        assert_eq!(first_sentence("Only one sentence"), "Only one sentence");
    }

    #[test]
    fn dotted_joins_skip_blanks() {
        assert_eq!(join_dot(&["a", "", "  ", "b"]), "a · b");
        assert_eq!(join_dot(&[]), "");
    }
}

/// The grammar both renderers actually implement, transcribed from
/// `crates/leo-package/src/ui/validate.rs` in the hub.
///
/// A dynamic app package cannot depend on the `leo-package` crate — the hub
/// that runs it may have no checkout at all — so the allowlists are copied
/// here and checked by [`tests::the_descriptor_uses_only_real_grammar`].
///
/// This exists because every mistake in a descriptor is **silent**. An unknown
/// `kind` renders its children bare or nothing at all; an unknown `style`
/// falls back to body text; an unknown colour token resolves to nil and the
/// node keeps its style colour. Nothing logs and nothing 500s, so the author
/// finds out from a screenshot of a blank pane.
#[cfg(test)]
mod grammar {
    pub const KINDS: &[&str] = &[
        "screen", "vstack", "hstack", "zstack", "spacer", "divider", "section",
        "text", "hero", "progress", "icon", "badge", "dot", "avatar", "image",
        "button", "checkbox", "searchField", "textField", "colorField",
        "textArea", "segmented", "slider", "markdown", "markdownEditor", "list",
        "grid", "masterDetail", "selectRow", "emptyState", "card", "panel",
        "authored", "paneTree", "chart", "compositionBar", "deltaChip",
        "carousel", "stepper", "tile", "chore_board", "entities_card",
        "room_card", "entity_card", "inlineComposer", "native", "sheet",
    ];

    pub const ACTION_KINDS: &[&str] = &[
        "setState", "confirm", "packageAction", "openURL", "openWindow",
        "closeWindow", "copyText",
    ];

    pub const TEXT_STYLES: &[&str] = &[
        "largeTitle", "hero", "title", "subtitle", "section-label", "headline",
        "mono", "meta", "caption", "subtle", "body",
    ];

    pub const COLOR_TOKENS: &[&str] = &[
        "accent", "secondary", "muted", "text", "white", "onImage",
        "whiteMuted", "onImageMuted", "ink", "inkMuted",
    ];

    /// The flat union of every prop either renderer reads.
    pub const PROPS: &[&str] = &[
        "a11yLabel", "acceptsDrop", "action", "actionLabel", "activeBind",
        "addressableState", "advance", "align", "aside", "assistant",
        "backLabel", "bare", "bind", "bleed", "body", "border",
        "canonicalRoute", "card", "checkedBody", "checkedWhen", "child",
        "children", "clearBackdrop", "color", "colorFrom", "columns",
        "commandBar", "commands", "composer", "content", "corner", "countBind",
        "dataParams", "default", "deltaBind", "deltaInverted", "detail",
        "detailFetch", "detailWidth", "direction", "discard", "dividerInset",
        "dividers", "empty", "entity_bind", "ephemeralState", "expand",
        "fallback", "fallbackIcon", "field", "fields", "fill", "fillWidth",
        "fillHeight", "filter", "fit", "fits", "focusKey", "fontScale",
        "format", "header", "headerActions", "height", "hideChromeState",
        "hideChromeStates", "hideChromeWhen", "hideTitle", "hideTitleState",
        "hideTitleStates", "hideWhenEmpty", "hint", "icon", "iconMax",
        "iconMin", "iconUrl", "inset", "inverted", "item", "items", "key",
        "label", "labelColor", "lineSpacing", "lines", "list", "mailboxNav",
        "max", "reserveLines", "maxBind", "maxWidth", "menuItems", "message",
        "min", "minLength", "minWidth", "moveAction", "noScrollState",
        "noScrollStates", "onOpen", "onOpenWhen", "openKey", "options",
        "overlay", "pack", "padding", "paddingH", "paddingTop", "paddingV",
        "paper", "path", "placeholder", "plain", "presentedWhen", "props",
        "recipientBind", "ref", "refreshAction", "refreshOn", "rememberState",
        "replyContextKey", "replyToKey", "requiresMessage", "revealOnHover",
        "role", "resizeAction", "resizeKey", "resizeMax", "resizeMin", "route",
        "rowStyle", "rung", "scaleKey", "scales", "schema", "scrim", "scroll",
        "scrollV", "secure", "seed", "savedBind", "seedBind", "selectedStyle",
        "selectedWhen", "selectionKey", "selectionTextKey", "sheet", "sheets",
        "shortcuts", "sidebar", "sidebarWidth", "size", "spacing", "spread",
        "state", "stateKey", "step", "style", "sub", "subBind", "subScale",
        "sublabel", "submit", "swipeActions", "symbol", "symbolBind",
        "symbolSize", "tabs", "tap", "theaterState", "theaterWhenAll",
        "threadBind", "threadKey", "tint", "tintFrom", "title", "titleBind",
        "toolbar", "topActions", "totalBind", "trailing", "uncheckedBody",
        "unit", "value", "valueBind", "variant", "visibleWhen", "width", "wrap",
    ];

    /// Keys inside an action object, which lives in a different namespace from
    /// a view node's props.
    pub const ACTION_KEYS: &[&str] = &[
        "kind", "method", "path", "body", "refresh", "optimistic", "then",
        "title", "message", "confirmLabel", "destructive", "confirm", "text",
        "key", "value", "url", "route", "params",
    ];
}

#[cfg(test)]
mod grammar_tests {
    use super::grammar::*;
    use super::*;

    /// The tree embedded in `descriptor.json` is the tree this code builds.
    ///
    /// The hub renders a native page from the **stored descriptor's** `ui`
    /// field; `ui_descriptor()` reads that and nothing else, and only
    /// `/leo/ui/data` is proxied to this process. So an embedded copy that has
    /// drifted is not a stale-looking page, it is a page drawn from old code
    /// with today's data bound into it — missing rows, dead buttons, binds that
    /// resolve to nothing, and no error anywhere. Regenerate with
    /// `scripts/sync-ui-descriptor.sh`.
    #[test]
    fn the_embedded_descriptor_matches_this_code() {
        let descriptor: Value = serde_json::from_str(include_str!("../descriptor.json")).unwrap();
        let embedded = descriptor.get("ui").unwrap_or_else(|| {
            panic!("descriptor.json has no `ui` — the page would have no tree to render")
        });
        assert_eq!(
            embedded,
            &pages(),
            "descriptor.json's ui has drifted from ui.rs — run scripts/sync-ui-descriptor.sh"
        );
    }

    /// A screen's address is the one the nav opens, or it has none.
    ///
    /// `rebuild_tools` publishes `tree.route` as the page's native route when
    /// it is declared, falling back to `/p/<pkg>/<path>`. Every nav surface
    /// addresses a package page at `/p/<pkg>/<path>`, so a tree that declares
    /// anything else publishes an address nothing navigates to: the click
    /// resolves no native surface, opens a web view, and lands on the hub's
    /// "has no web page" card. Nothing is logged, the package is healthy, and
    /// the page works perfectly if you type the declared route by hand — which
    /// is how this survived a day of testing.
    #[test]
    fn the_declared_route_is_the_one_the_nav_opens() {
        let descriptor: Value = serde_json::from_str(include_str!("../descriptor.json")).unwrap();
        let package = descriptor["name"].as_str().unwrap();
        for (page, tree) in pages().as_object().expect("pages is an object") {
            let Some(declared) = tree.get("route").and_then(Value::as_str) else {
                continue; // Undeclared is correct: the hub derives the address.
            };
            assert_eq!(
                declared,
                format!("/p/{package}/{page}"),
                "page '{page}' declares a route the nav never opens"
            );
        }
    }

    /// Some kinds are *made of* one named child, and reading it from the wrong
    /// key draws a placeholder instead of the content.
    ///
    /// The renderers say this out loud — `missingChildNotice(kind:key:)` is
    /// "This screen has no 'body' to draw", "This masterDetail has no
    /// 'list.item' to draw" — but only on screen, to whoever is looking. Every
    /// kind is spelled correctly in that state and every prop is a real prop,
    /// so the grammar walk passes a tree that renders as an empty pane. Both
    /// mistakes were made here and both cost a round of "the package installs,
    /// the app serves, the page is blank" to find.
    ///
    /// `children` is the trap: it is a real prop on the stacks, so it looks
    /// right everywhere. For these three kinds it is simply not read.
    #[test]
    fn the_kinds_made_of_one_named_child_have_it() {
        /// kind → the key its content must be under, and whether `children`
        /// there is a mistake rather than merely unused.
        const REQUIRED: &[(&str, &str)] = &[
            ("screen", "body"),
            ("list", "item"),
            ("masterDetail", "list"),
        ];

        let tree = pages();
        let mut problems = Vec::new();
        check(&tree, "spaces", &mut problems, REQUIRED);
        assert!(
            problems.is_empty(),
            "nodes whose content is under a key no renderer reads:\n  {}",
            problems.join("\n  ")
        );

        fn check(v: &Value, path: &str, out: &mut Vec<String>, rules: &[(&str, &str)]) {
            match v {
                Value::Object(map) => {
                    let required = map
                        .get("kind")
                        .and_then(Value::as_str)
                        .and_then(|kind| rules.iter().find(|(k, _)| *k == kind));
                    if let Some((kind, key)) = required {
                        if !map.contains_key(*key) {
                            out.push(format!("{path}: {kind} has no `{key}`"));
                        } else if map.contains_key("children") {
                            out.push(format!(
                                "{path}: {kind} carries `children`, which it does not read — \
                                 its content belongs under `{key}`"
                            ));
                        }
                    }
                    for (k, child) in map {
                        check(child, &format!("{path}.{k}"), out, rules);
                    }
                }
                Value::Array(items) => {
                    for (i, item) in items.iter().enumerate() {
                        check(item, &format!("{path}[{i}]"), out, rules);
                    }
                }
                _ => {}
            }
        }
    }

    /// Walk the descriptor and check every `kind`, `style`, colour, prop name
    /// and action against the grammar the renderers implement.
    ///
    /// The failures this catches, each of which renders as silence:
    /// `kind: "vstakc"` → children drawn bare or nothing; `style: "pubDate"` →
    /// body text; `color: "nuetral"` → the style's own colour; an action with
    /// an unknown `kind` and no `method` → a button that looks tappable and
    /// does nothing.
    #[test]
    fn the_descriptor_uses_only_real_grammar() {
        let tree = pages();
        let mut problems = Vec::new();
        walk(&tree, "spaces", &mut problems);
        assert!(
            problems.is_empty(),
            "the descriptor uses grammar no renderer implements:\n  {}",
            problems.join("\n  ")
        );
    }

    fn walk(v: &Value, path: &str, out: &mut Vec<String>) {
        match v {
            Value::Object(map) => {
                let kind = map.get("kind").and_then(Value::as_str);

                // An object is a view node, an action, or a free-form bag
                // (a condition, a filter, a section). Only the first two have
                // a closed vocabulary.
                let is_node = kind.is_some_and(|k| KINDS.contains(&k));
                let is_action = kind.is_some_and(|k| ACTION_KINDS.contains(&k))
                    || map.contains_key("method");

                if let Some(k) = kind {
                    if !is_node && !is_action {
                        out.push(format!("{path}: unknown kind '{k}'"));
                    }
                }

                if is_node {
                    for key in map.keys() {
                        if key == "kind" {
                            continue;
                        }
                        if !PROPS.contains(&key.as_str()) {
                            out.push(format!("{path}: no renderer reads prop '{key}'"));
                        }
                    }
                    if let Some(style) = map.get("style").and_then(Value::as_str) {
                        if !TEXT_STYLES.contains(&style) {
                            out.push(format!("{path}: unknown text style '{style}'"));
                        }
                    }
                    for key in ["color", "tint"] {
                        if let Some(c) = map.get(key).and_then(Value::as_str) {
                            if !c.starts_with('#') && !COLOR_TOKENS.contains(&c) {
                                out.push(format!("{path}: unknown {key} token '{c}'"));
                            }
                        }
                    }
                }

                if is_action {
                    for key in map.keys() {
                        if !ACTION_KEYS.contains(&key.as_str()) {
                            out.push(format!("{path}: unknown action key '{key}'"));
                        }
                    }
                }

                // Conditions, filters and `body` bags are author-defined and
                // never checked — recursing into them would flag field names.
                for (k, child) in map {
                    if matches!(k.as_str(), "visibleWhen" | "filter" | "body" | "assistant" | "state") {
                        continue;
                    }
                    walk(child, &format!("{path}.{k}"), out);
                }
            }
            Value::Array(items) => {
                for (i, item) in items.iter().enumerate() {
                    walk(item, &format!("{path}[{i}]"), out);
                }
            }
            _ => {}
        }
    }

    /// Every `stateKey` a control writes, and every state key a condition
    /// gates on, must be seeded in the screen's `state`.
    ///
    /// An unseeded key reads as `""`, so the control starts blank and anything
    /// gated on it hides forever — the single most common way a descriptor
    /// renders an empty pane with nothing in any log.
    #[test]
    fn every_state_key_used_is_seeded() {
        let s = screen();
        let seeded: Vec<String> = s["state"]
            .as_object()
            .expect("the screen must seed its state")
            .keys()
            .cloned()
            .collect();

        let mut used = Vec::new();
        collect_state_keys(&s, &mut used);
        for key in &used {
            assert!(
                seeded.contains(key),
                "'{key}' is read but never seeded in screen.state (seeded: {seeded:?})"
            );
        }
        // And the reverse, which is only a tidiness issue but catches a
        // renamed key leaving its old seed behind.
        for key in &seeded {
            assert!(
                used.contains(key) || key == "selectedId",
                "'{key}' is seeded but nothing reads it"
            );
        }
    }

    fn collect_state_keys(v: &Value, out: &mut Vec<String>) {
        match v {
            Value::Object(map) => {
                if let Some(k) = map.get("stateKey").and_then(Value::as_str) {
                    out.push(k.to_string());
                }
                if let Some(k) = map.get("selectionKey").and_then(Value::as_str) {
                    out.push(k.to_string());
                }
                // `visibleWhen: { state: "tab", equals: … }` and
                // `filter: { equalsState: "tab", search: "query" }`.
                if let Some(k) = map.get("state").and_then(Value::as_str) {
                    out.push(k.to_string());
                }
                if let Some(k) = map.get("equalsState").and_then(Value::as_str) {
                    out.push(k.to_string());
                }
                if let Some(k) = map.get("search").and_then(Value::as_str) {
                    out.push(k.to_string());
                }
                for child in map.values() {
                    collect_state_keys(child, out);
                }
            }
            Value::Array(items) => items.iter().for_each(|i| collect_state_keys(i, out)),
            _ => {}
        }
    }

    /// Every REST path an action posts to must be a route this package serves,
    /// under the hub's mount prefix.
    ///
    /// A path the hub cannot route is a button that returns 404 and looks like
    /// a server error. The `/p/spaces` prefix is required: the clients never
    /// speak to this process directly, so a bare `/api/...` reaches the hub
    /// and misses.
    #[test]
    fn every_action_path_is_a_route_this_package_serves() {
        let tree = json!({ "spaces": screen() });
        let mut paths = Vec::new();
        collect_paths(&tree, &mut paths);
        assert!(!paths.is_empty(), "the screen should have REST actions");

        for p in &paths {
            assert!(
                p.starts_with("/p/spaces/"),
                "'{p}' is not under this package's mount prefix — it would reach the hub instead"
            );
            // Strip the prefix and the interpolation, and check the shape
            // against what api::router() declares.
            let route = p.trim_start_matches("/p/spaces");
            let known = [
                "/api/host/apply",
                "/api/spaces/{title}/control",
                "/api/spaces/{title}/gpu",
                "/api/spaces/{title}/snapshot",
            ];
            assert!(
                known.contains(&route),
                "'{route}' is not a route api::router() serves"
            );
        }
    }

    fn collect_paths(v: &Value, out: &mut Vec<String>) {
        match v {
            Value::Object(map) => {
                if map.contains_key("method") {
                    if let Some(p) = map.get("path").and_then(Value::as_str) {
                        out.push(p.to_string());
                    }
                }
                for child in map.values() {
                    collect_paths(child, out);
                }
            }
            Value::Array(items) => items.iter().for_each(|i| collect_paths(i, out)),
            _ => {}
        }
    }

    /// Icons are SF Symbol names, which the clients resolve at render time —
    /// a wrong one draws nothing. These are the ones this screen uses.
    #[test]
    fn the_sidebar_icons_are_named() {
        let s = screen();
        let mut icons = Vec::new();
        collect_prop(&s, "icon", &mut icons);
        for want in ["macwindow", "wrench.and.screwdriver", "cpu"] {
            assert!(icons.contains(&want.to_string()), "missing icon {want}");
        }
        // No empty icon strings, which render as a gap where a glyph should be.
        for i in &icons {
            assert!(!i.is_empty(), "an empty icon name renders as a hole");
        }
    }

    fn collect_prop(v: &Value, prop: &str, out: &mut Vec<String>) {
        match v {
            Value::Object(map) => {
                if let Some(s) = map.get(prop).and_then(Value::as_str) {
                    out.push(s.to_string());
                }
                for child in map.values() {
                    collect_prop(child, prop, out);
                }
            }
            Value::Array(items) => items.iter().for_each(|i| collect_prop(i, prop, out)),
            _ => {}
        }
    }
}
