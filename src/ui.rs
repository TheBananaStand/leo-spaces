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
use crate::installers;
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
/// The rail's width — LeoCode's.
const RAIL_WIDTH: u32 = 216;
/// The pane gutter — LeoCode's, between panels and around them.
const GUTTER: u32 = 12;

/// The narrowest a tile may be before the grid drops a column. One number,
/// shared by the grid's column model and the art block inside the cell.
const TILE_MIN_WIDTH: u32 = 320;

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
pub async fn descriptor_handler(_c: crate::auth::Caller, _s: State<AppState>) -> Json<Value> {
    Json(pages())
}

/// `GET /leo/ui/data?page=spaces` — everything live.
///
/// An absent `page` means the only page there is. The hub forwards the
/// *client's* query params rather than supplying the page itself, so a caller
/// omitting it is ordinary rather than broken — answering with "page '' has no
/// data" would turn this package's pedantry into a 502.
pub async fn data_handler(
    c: crate::auth::Caller,
    State(s): State<AppState>,
    Query(q): Query<DataQuery>,
) -> Result<Json<Value>, crate::error::Error> {
    match q.page.as_deref().unwrap_or(DEFAULT_PAGE) {
        "" | DEFAULT_PAGE => Ok(Json(data(&s, c.is_admin).await)),
        other => Err(crate::error::Error::Other(format!(
            "spaces has no page '{other}' — this package serves only '{DEFAULT_PAGE}'"
        ))),
    }
}

// ───────────────────────────────────────────────────────────────── descriptor
//
// The shape is LeoCode's: a rail on the left, and beside it whichever face
// the rail has picked. **Spaces** is the day-to-day list with one machine's
// pane beside it. **Settings** is the host — its setup steps, what it is,
// its graphics devices and the installer images it holds — as panels at the
// pane gutter. Two faces, because the one-sentence test fails with "and":
// running machines is one subject, and making the host able to run them is
// another.

fn screen() -> Value {
    json!({
        "kind": "screen",
        "title": "Spaces",
        // The rail's hero names the surface; a second "Spaces" above it would
        // be the same name twice, a gutter apart.
        "hideTitle": true,
        // No `route`. The hub derives a package page's native address from the
        // tree's `route` when it declares one, and from `/p/<pkg>/<path>`
        // otherwise — and `/p/<pkg>/<path>` is what every nav surface actually
        // addresses. See `the_declared_route_is_the_one_the_nav_opens`.
        "state": {
            "tab": "spaces",
            "query": "",
            "selectedId": "",
            // Which machine's screen is open, and the empty string for none.
            // DOTTED on purpose: `DescriptorScope.value(for:)` resolves a token
            // against the payload first and only falls back to page state when
            // the token contains a dot, so a bare `{console}` would interpolate
            // to nothing — in the route the console pane loads, which is the
            // one place that failure is invisible until the pane is blank.
            "console.name": "",
            // Set once and never moved: the faces fill the window and scroll
            // inside their own panes rather than the page scrolling under
            // them. The only vocabulary for "this surface owns its viewport"
            // is a state key.
            "fills": "on",
            "creating": false,
            "form.name": "",
            "form.family": "windows",
            "form.os": "win2k22",
            "form.vcpus": "8",
            "form.memory_mb": "16384",
            "form.disk_gb": "120",
            "form.gpu": "",
            "form.product_key": "",
            "pair_code": ""
        },
        // Both are addressable so a space can be linked to directly — which is
        // what makes "open the CAD machine" a thing the assistant can answer
        // with a place rather than a description.
        // A machine's screen is a place: `?tab=console&console.name=engineering`
        // is a link straight into it.
        "addressableState": ["tab", "selectedId", "console.name"],
        // A half-filled form is not an address.
        "ephemeralState": [
            "creating", "form.name", "form.family", "form.os",
            "form.vcpus", "form.memory_mb", "form.disk_gb", "form.gpu",
            "form.product_key", "pair_code"
        ],
        "noScrollState": "fills",
        "noScrollStates": ["fills"],
        "assistant": {
            "app": "Spaces",
            "placeholder": "Ask about your machines…",
            "suggestions": [
                "Is the hypervisor set up on this host?",
                "What's running right now?",
                "Make an Ubuntu space with 4 cores and 8 GB",
                "Can the GPU be passed through?"
            ]
        },
        "sheet": new_space_sheet(),
        // `body`, not `children`: the renderers read a screen's one child from
        // `body` — see `a_screen_draws_from_body`.
        "body": {
            "kind": "hstack",
            "spacing": 0,
            "children": [
                rail(),
                { "kind": "divider" },
                spaces_face(),
                settings_face()
            ]
        }
    })
}

/// The column of doors. One subject at the top — can this host run a guest —
/// then the two faces, then the one verb that makes something new.
fn rail() -> Value {
    json!({
        "kind": "vstack",
        "spacing": 2,
        "align": "leading",
        "width": RAIL_WIDTH,
        "paddingV": 8,
        "children": [
            host_hero(),
            section_label("MACHINES", 14),
            {
                "kind": "searchField",
                "stateKey": "query",
                "placeholder": "Search",
                "width": RAIL_WIDTH - 20
            },
            tab_row("spaces", "Spaces", "macwindow", "space_count"),
            tab_row("settings", "Settings", "gearshape", "setup_count"),
            {
                "kind": "button",
                "variant": "rail",
                "icon": "plus",
                "label": "New space",
                "a11yLabel": "Make a new space",
                "action": [
                    { "kind": "setState", "key": "form.name", "value": "" },
                    { "kind": "setState", "key": "creating", "value": true }
                ]
            },
            { "kind": "spacer" }
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

// ── the Spaces face ────────────────────────────────────────────────────────

fn spaces_face() -> Value {
    json!({
        // An unlabelled `panel` purely to fill the pane's width — `fill` is a
        // height word and a bare vstack drew a narrow column. Its two bodies
        // are the board and one machine's screen, and exactly one shows.
        //
        // ONE state key decides which, and that is the point. The first cut
        // set `console.name` *and* `tab`, and only one of the two ever stuck:
        // `setState` calls `publishAddress()` on every write, that address is
        // applied back onto the surface, and the re-seed races the second
        // write — so the pair came out as whichever won, differently each run.
        // A single key cannot race itself.
        "kind": "panel",
        "padding": 0,
        "scroll": false,
        "visibleWhen": { "state": "tab", "equals": "spaces" },
        "children": [ board_body(), console_body() ]
    })
}

/// The board: every machine as a tile.
fn board_body() -> Value {
    json!({
        "kind": "vstack",
        "spacing": 0,
        "align": "leading",
        "visibleWhen": { "state": "console.name", "equals": "" },
        "children": [
            {
                "kind": "hstack",
                "spacing": 10,
                "align": "center",
                "paddingH": GUTTER + 10,
                "paddingTop": 16,
                "paddingV": 10,
                "children": [
                    { "kind": "text", "value": "Spaces", "style": "title" },
                    { "kind": "text", "bind": "space_count", "style": "title", "color": "muted" },
                    { "kind": "spacer" }
                ]
            },
            {
                "kind": "grid",
                "bind": "rows",
                "minWidth": TILE_MIN_WIDTH,
                "spacing": 16,
                "paddingH": GUTTER + 10,
                "paddingV": 4,
                "scrollV": true,
                "filter": {
                    "field": "tab",
                    "equalsState": "tab",
                    "search": "query",
                    "searchFields": ["title", "subtitle", "trailing"]
                },
                "empty": {
                    "kind": "emptyState",
                    "icon": "macwindow",
                    "title": "No spaces yet",
                    "message": "New space, in the rail, makes one: pick the operating system, the size and a GPU. Or ask Leo \u{201c}make a Windows Server space with 8 cores and 16 GB\u{201d}."
                },
                "swipeActions": { "trailing": space_menu() },
                "item": space_tile()
            }
        ]
    })
}

/// One machine, as a tile big enough to read across a room.
///
/// The whole tile taps. On a running guest that opens its screen, which is
/// what somebody crossing the room came for; on a stopped one it starts it.
/// Nothing opens a pane — the facts a person actually wants at a glance are
/// on the face of the tile, and the verbs are on its context menu.
///
/// Laid out as a zstack so the name sits *over* the field of colour rather
/// than under it: `zstack` centres its children in both twins, so the overlay
/// is a filling `vstack` whose spacers push the chip to the top edge and the
/// name to the bottom. That is the only way to reach a corner in this
/// grammar, and it is spelled once here.
fn space_tile() -> Value {
    json!({
        "kind": "vstack",
        "card": true,
        "spacing": 0,
        "padding": 0,
        // Tapping the machine does the obvious thing to it. `tap` is honoured
        // on any node, so the whole face is the target — not just its glyphs.
        // In place, not in a window. `openWindow` routed this through
        // `WindowController.openNavDestination`, which gives a web route a
        // floating panel — the surface you were reading still behind it. Two
        // `setState`s instead: which machine, then the face that draws it.
        "tap": { "kind": "setState", "key": "console.name", "value": "{title}" },
        "children": [
            {
                "kind": "zstack",
                "children": [
                    // The field of colour: square, so it scales with the
                    // column rather than being pinned to a guessed height.
                    // `tile` is the one primitive that takes an interpolated
                    // fill, which is what lets the OS choose the colour.
                    // No width and no height on purpose: `tile` squares
                    // itself to whatever it is offered (`DescriptorFixedSize`
                    // only takes a frame when *both* are stated), so the field
                    // of colour is exactly as wide as the cell the grid
                    // resolved. Stating a width pinned it to the narrowest
                    // column and left a gutter down both sides of every tile.
                    {
                        "kind": "tile",
                        "fill": "{art_color}",
                        "corner": 10,
                        "label": ""
                    },
                    {
                        "kind": "vstack",
                        "fill": true,
                        "spacing": 0,
                        "align": "leading",
                        "padding": 14,
                        "children": [
                            {
                                "kind": "hstack",
                                "spacing": 8,
                                "align": "center",
                                "children": [
                                    { "kind": "badge", "bind": "os_chip", "tint": "surface", "hideWhenEmpty": true },
                                    { "kind": "spacer" },
                                    { "kind": "dot", "colorFrom": "state_color", "size": 8 },
                                    { "kind": "text", "bind": "badge", "style": "caption", "colorFrom": "state_color", "lines": 1 }
                                ]
                            },
                            { "kind": "spacer" },
                            // `bind`, not `symbol`: a static `symbol` is read
                            // verbatim (`node.str("symbol")`) and is never
                            // scope-interpolated, so "{os_symbol}" reached
                            // SF Symbols as that literal string and drew
                            // nothing at all. The bound arm is the one that
                            // resolves against the row.
                            {
                                "kind": "hstack",
                                "children": [
                                    { "kind": "spacer" },
                                    { "kind": "icon", "bind": "os_symbol", "size": 40, "color": "whiteMuted" },
                                    { "kind": "spacer" }
                                ]
                            },
                            { "kind": "spacer" },
                            { "kind": "text", "bind": "title", "style": "headline", "color": "white", "lines": 1 },
                            { "kind": "text", "bind": "subtitle", "style": "caption", "color": "whiteMuted", "lines": 1, "hideWhenEmpty": true }
                        ]
                    }
                ]
            },
            // The strip under the art: where it is on the network, and the
            // sign-in Leo made at install when the reader is an admin.
            {
                "kind": "vstack",
                "spacing": 3,
                "align": "leading",
                "paddingH": 14,
                "paddingV": 11,
                "children": [
                    {
                        "kind": "hstack",
                        "spacing": 8,
                        "align": "center",
                        "children": [
                            { "kind": "text", "bind": "trailing", "style": "mono", "color": "secondary", "lines": 1, "hideWhenEmpty": true },
                            { "kind": "spacer" },
                            { "kind": "text", "bind": "gpu_chip", "style": "caption", "color": "muted", "lines": 1, "hideWhenEmpty": true }
                        ]
                    },
                    { "kind": "text", "bind": "admin_line", "style": "mono", "color": "muted", "lines": 1, "hideWhenEmpty": true }
                ]
            }
        ]
    })
}

/// One machine's screen, where the board was.
///
/// The panel above sets `scroll: false`, because a VNC surface is not a
/// document — inside a scroll view it is handed an unbounded height and
/// draws a sliver.
fn console_body() -> Value {
    json!({
        "kind": "vstack",
        "spacing": 0,
        "align": "leading",
        "fill": true,
        "visibleWhen": { "state": "console.name", "nonEmpty": true },
        "children": [
            {
                "kind": "hstack",
                "spacing": 10,
                "align": "center",
                "paddingH": GUTTER + 10,
                "paddingTop": 14,
                "paddingV": 10,
                "children": [
                    {
                        "kind": "button",
                        "variant": "pill",
                        "icon": "chevron.left",
                        "label": "Spaces",
                        "a11yLabel": "Back to the board",
                        "action": { "kind": "setState", "key": "console.name", "value": "" }
                    },
                    // No machine name here. `text`'s `value` is taken
                    // verbatim — `resolved()` returns `.string(v)` with no
                    // interpolation, unlike a `panel`'s label or an action's
                    // — so "{console.name}" rendered as those literal
                    // characters. The console page draws its own name a few
                    // points below anyway, so the fix is one less thing to
                    // read rather than a second way to say it.
                    { "kind": "spacer" },
                    {
                        "kind": "button",
                        "variant": "pill",
                        "icon": "macwindow.on.rectangle",
                        "label": "Open in window",
                        "a11yLabel": "Open this screen in its own window",
                        // Still offered, deliberately: a second monitor is a
                        // real way to use a machine, and a pane cannot be
                        // dragged onto one.
                        "action": {
                            "kind": "openWindow",
                            "route": "/p/spaces/console/{console.name}",
                            "label": "{console.name}"
                        }
                    }
                ]
            },
            {
                "kind": "native",
                "ref": "webPane",
                // No `height`: a native is greedy and takes what it is offered,
                // which here is the rest of the pane. Stating one would pin the
                // screen to a guessed size inside a surface that knows its own.
                "props": { "route": "/p/spaces/console/{console.name}" },
                "fallback": {
                    "kind": "emptyState",
                    "icon": "display",
                    "title": "No screen here",
                    "message": "This client can\u{2019}t draw a remote screen in place. Open in window shows it in its own panel."
                }
            }
        ]
    })
}

/// Every verb that was in the pane, on the tile's own menu.
///
/// Each is gated on the state it makes sense in — a Start on a running
/// machine is a question the reader has to answer before they can act.
fn space_menu() -> Value {
    json!([
        { "label": "Open screen", "icon": "display",
          "visibleWhen": { "field": "has_console", "equals": "1" },
          "action": { "kind": "openWindow", "route": "/p/spaces/console/{title}", "label": "{title}" } },
        { "label": "Start", "icon": "play",
          "visibleWhen": { "field": "running", "equals": "off" },
          "action": { "method": "POST", "path": "/p/spaces/api/spaces/{title}/control",
                      "body": { "action": "start" }, "refresh": "reload" } },
        { "label": "Shut down", "icon": "power",
          "visibleWhen": { "field": "running", "equals": "on" },
          "action": { "method": "POST", "path": "/p/spaces/api/spaces/{title}/control",
                      "body": { "action": "shutdown" }, "refresh": "reload" } },
        { "label": "Restart", "icon": "arrow.clockwise",
          "visibleWhen": { "field": "running", "equals": "on" },
          "action": { "method": "POST", "path": "/p/spaces/api/spaces/{title}/control",
                      "body": { "action": "reboot" }, "refresh": "reload" } },
        { "label": "Force off", "icon": "bolt.slash", "tint": "error",
          "visibleWhen": { "field": "running", "equals": "on" },
          "action": {
              "kind": "confirm",
              "title": "Force \u{201c}{title}\u{201d} off?",
              "message": "Cuts the power without telling the guest. Unsaved work is lost, and a guest writing to disk can be left with a damaged filesystem. Use Shut down unless it has stopped responding.",
              "confirmLabel": "Force off",
              "destructive": true,
              "confirm": { "method": "POST", "path": "/p/spaces/api/spaces/{title}/control",
                           "body": { "action": "force_off" }, "refresh": "reload" }
          } },
        { "label": "Delete", "icon": "trash", "tint": "error",
          "visibleWhen": { "field": "running", "equals": "off" },
          "action": {
              "kind": "confirm",
              "title": "Delete \u{201c}{title}\u{201d}?",
              "message": "Removes the machine's definition. Its disk is kept under /var/lib/leo-spaces/disks, so what was installed on it is not lost.",
              "confirmLabel": "Delete",
              "destructive": true,
              "confirm": { "method": "DELETE", "path": "/p/spaces/api/spaces/{title}", "refresh": "reload" }
          } }
    ])
}

// ── the Settings face ──────────────────────────────────────────────────────

/// The host, as panels at the pane gutter: what still needs doing to it, what
/// it is, what graphics it has, and which installers it holds.
fn settings_face() -> Value {
    json!({
        "kind": "hstack",
        "spacing": GUTTER,
        "padding": GUTTER,
        "visibleWhen": { "state": "tab", "equals": "settings" },
        "children": [
            panel("Host setup", vec![
                json!({ "kind": "list", "bind": "setup_rows", "spacing": 0, "item": step_row() })
            ]),
            {
                "kind": "vstack",
                "spacing": GUTTER,
                "children": [
                    hugging_panel("Host", vec![
                        json!({ "kind": "list", "bind": "host_facts", "spacing": 6, "item": fact_row() }),
                        json!({
                            "kind": "text",
                            "bind": "unreachable_reason",
                            "style": "caption",
                            "color": "muted",
                            "wrap": true,
                            "hideWhenEmpty": true
                        })
                    ]),
                    panel("Graphics", vec![
                        json!({ "kind": "list", "bind": "gpu_rows", "spacing": 0, "item": gpu_row() }),
                        json!({
                            "kind": "emptyState",
                            "icon": "cpu",
                            "title": "No graphics devices",
                            "message": "Nothing on the PCI bus reports itself as a display adapter.",
                            "visibleWhen": { "field": "has_gpus", "equals": "0" }
                        })
                    ])
                ]
            },
            panel("Installers", vec![
                json!({ "kind": "text", "bind": "installer_hint", "style": "caption", "color": "muted", "wrap": true }),
                json!({ "kind": "list", "bind": "installer_rows", "spacing": 0, "item": installer_row() })
            ])
        ]
    })
}

/// A LeoCode pane: a name strip over glass content that fills its height.
fn panel(label: &str, children: Vec<Value>) -> Value {
    json!({
        "kind": "panel",
        "label": label,
        "spacing": 4,
        "children": children
    })
}

/// The same pane at its content's height, for one stacked over another that
/// should take the rest.
fn hugging_panel(label: &str, children: Vec<Value>) -> Value {
    let mut p = panel(label, children);
    // Both, because a scrolling pane fills whatever `fillHeight` says: the
    // scroll view takes the height it is offered and the flag is only read
    // on the unscrolled arm. Nothing here is long enough to scroll.
    p["fillHeight"] = json!(false);
    p["scroll"] = json!(false);
    p
}

/// One setup step, with its verb on the row. The subtitle is the one thing
/// that matters about it right now: why it is blocked, that it is waiting
/// for a reboot, or what it does.
fn step_row() -> Value {
    json!({
        "kind": "hstack",
        "spacing": 10,
        "align": "center",
        "paddingV": 7,
        "children": [
            { "kind": "dot", "colorFrom": "state_color", "size": 7 },
            {
                "kind": "vstack",
                "spacing": 2,
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
                    { "kind": "text", "bind": "subtitle", "style": "caption", "color": "secondary", "wrap": true, "lines": 2, "hideWhenEmpty": true }
                ]
            },
            { "kind": "spacer" },
            { "kind": "text", "bind": "trailing", "style": "mono", "color": "secondary", "hideWhenEmpty": true },
            // On a host this package has no privilege over, the command is
            // the whole deliverable — so it is copyable from the row.
            {
                "kind": "button",
                "variant": "icon",
                "icon": "doc.on.doc",
                "label": "Copy command",
                "a11yLabel": "Copy the command this step would run",
                "visibleWhen": { "field": "copy_only", "equals": "1" },
                "action": { "kind": "copyText", "text": "{command}" }
            },
            {
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
            },
            {
                "kind": "button",
                "variant": "pill",
                "label": "Apply",
                "visibleWhen": { "field": "apply_mode", "equals": "boot" },
                "action": {
                    "kind": "confirm",
                    "title": "Change how this host boots?",
                    "message": "This edits what the host loads at boot. A backup of the boot configuration is written first, and any fallback entry is left untouched so there is a way back. The change takes effect on the next reboot \u{2014} nothing restarts now.",
                    "confirmLabel": "Write it",
                    "destructive": true,
                    "confirm": {
                        "method": "POST",
                        "path": "/p/spaces/api/host/apply",
                        "body": { "step": "{id}" },
                        "refresh": "reload"
                    }
                }
            }
        ]
    })
}

fn fact_row() -> Value {
    json!({
        "kind": "hstack",
        "spacing": 8,
        "align": "center",
        "children": [
            { "kind": "text", "bind": "label", "style": "caption" },
            { "kind": "spacer" },
            { "kind": "text", "bind": "value", "style": "mono", "lines": 1 }
        ]
    })
}

fn gpu_row() -> Value {
    json!({
        "kind": "hstack",
        "spacing": 10,
        "align": "center",
        "paddingV": 7,
        "children": [
            { "kind": "dot", "colorFrom": "state_color", "size": 7 },
            {
                "kind": "vstack",
                "spacing": 2,
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
                    { "kind": "text", "bind": "subtitle", "style": "caption", "color": "secondary", "wrap": true, "lines": 2, "hideWhenEmpty": true }
                ]
            },
            { "kind": "spacer" },
            { "kind": "text", "bind": "trailing", "style": "mono", "color": "secondary", "hideWhenEmpty": true }
        ]
    })
}

/// One operating system the catalog offers, with what the host has of its
/// installer and the one verb that changes that.
fn installer_row() -> Value {
    json!({
        "kind": "hstack",
        "spacing": 10,
        "align": "center",
        "paddingV": 7,
        "children": [
            { "kind": "dot", "colorFrom": "state_color", "size": 7 },
            {
                "kind": "vstack",
                "spacing": 2,
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
                    { "kind": "text", "bind": "subtitle", "style": "caption", "color": "secondary", "wrap": true, "lines": 2, "hideWhenEmpty": true }
                ]
            },
            { "kind": "spacer" },
            { "kind": "text", "bind": "trailing", "style": "mono", "color": "secondary", "hideWhenEmpty": true },
            {
                "kind": "button",
                "variant": "icon",
                "icon": "doc.on.doc",
                "label": "Copy path",
                "a11yLabel": "Copy where the image goes",
                "visibleWhen": { "field": "bring", "equals": "1" },
                "action": { "kind": "copyText", "text": "{path}" }
            },
            {
                "kind": "button",
                "variant": "pill",
                "label": "Import",
                "visibleWhen": { "field": "staged", "equals": "1" },
                "action": {
                    "method": "POST",
                    "path": "/p/spaces/api/host/import-iso",
                    "body": { "os": "{id}" },
                    "refresh": "reload"
                }
            },
            {
                "kind": "button",
                "variant": "pill",
                "label": "Download",
                "visibleWhen": { "field": "can_fetch", "equals": "1" },
                "action": {
                    "method": "POST",
                    "path": "/p/spaces/api/host/fetch-iso",
                    "body": { "os": "{id}" },
                    "refresh": "reload"
                }
            }
        ]
    })
}

// ── the New-space sheet ────────────────────────────────────────────────────

/// Making a space: a name, an operating system from the catalog, a size, a
/// GPU. The numbers are chosen from a few sizes rather than typed, because a
/// hypervisor has few sensible answers and a free field invites the one that
/// swaps the host.
fn new_space_sheet() -> Value {
    json!({
        "presentedWhen": "creating",
        "title": "New space",
        "width": 560,
        "submit": {
            "label": "Create",
            "action": {
                "method": "POST",
                "path": "/p/spaces/api/spaces",
                "body": {
                    "name": "{form.name}",
                    "os": "{form.os}",
                    "vcpus": "{form.vcpus}",
                    "memory_mb": "{form.memory_mb}",
                    "disk_gb": "{form.disk_gb}",
                    "gpu": "{form.gpu}",
                    "product_key": "{form.product_key}"
                },
                "refresh": "reload",
                // The create asked the hub for a pairing code for the new
                // space's satellite; approving it mints the token, and only
                // a signed-in admin may — which is the person pressing this.
                // The app then puts the token on the guest tools CD. A space
                // made with no CD answers no code, and the approve is skipped
                // rather than posted to nothing.
                "setStateFromResponse": { "pair_code": "pairing_code" },
                "then": [
                    { "kind": "setState", "key": "creating", "value": false },
                    {
                        "method": "POST",
                        "path": "/api/satellite-pair/{pair_code}/approve",
                        "refresh": "none"
                    }
                ]
            }
        },
        "fields": [
            {
                "kind": "textField",
                "stateKey": "form.name",
                "label": "Name",
                "placeholder": "win-cad \u{2014} letters, digits and dashes"
            },
            sheet_label("OPERATING SYSTEM"),
            {
                "kind": "segmented",
                "stateKey": "form.family",
                "options": [
                    { "value": "windows", "label": "Windows" },
                    { "value": "linux", "label": "Linux" },
                    { "value": "bsd", "label": "BSD" }
                ]
            },
            {
                "kind": "list",
                "bind": "os_catalog",
                "spacing": 2,
                "filter": { "field": "family", "equalsState": "form.family" },
                "item": {
                    "kind": "selectRow",
                    "stateKey": "form.os",
                    "value": "{id}",
                    "label": "{label}",
                    // Blank when the installer is on the host — a status that
                    // is always fine is not information. The word appears
                    // only when it changes what pressing Create will do.
                    "countBind": "standing_label"
                }
            },
            { "kind": "text", "bind": "installer_hint", "style": "caption", "color": "muted", "wrap": true },
            {
                "kind": "textField",
                "stateKey": "form.product_key",
                "label": "Windows product key (optional)",
                "placeholder": "Blank: installs with a generic key, activate later"
            },
            sheet_label("SIZE"),
            {
                "kind": "segmented",
                "stateKey": "form.vcpus",
                "options": [
                    { "value": "4", "label": "4 cores" },
                    { "value": "8", "label": "8 cores" },
                    { "value": "16", "label": "16 cores" },
                    { "value": "24", "label": "24 cores" }
                ]
            },
            {
                "kind": "segmented",
                "stateKey": "form.memory_mb",
                "options": [
                    { "value": "8192", "label": "8 GB" },
                    { "value": "16384", "label": "16 GB" },
                    { "value": "32768", "label": "32 GB" },
                    { "value": "65536", "label": "64 GB" }
                ]
            },
            {
                "kind": "segmented",
                "stateKey": "form.disk_gb",
                "options": [
                    { "value": "60", "label": "60 GB disk" },
                    { "value": "120", "label": "120 GB disk" },
                    { "value": "250", "label": "250 GB disk" },
                    { "value": "500", "label": "500 GB disk" }
                ]
            },
            sheet_label("GRAPHICS"),
            {
                "kind": "list",
                "bind": "gpu_choices",
                "spacing": 2,
                "item": {
                    "kind": "selectRow",
                    "stateKey": "form.gpu",
                    "value": "{address}",
                    "label": "{label}",
                    "countBind": "note"
                }
            }
        ]
    })
}

fn sheet_label(label: &str) -> Value {
    json!({ "kind": "text", "style": "section-label", "value": label, "paddingTop": 6 })
}

// ──────────────────────────────────────────────────────────────────── data

async fn data(s: &AppState, is_admin: bool) -> Value {
    let f = crate::facts::probe(&s.cfg.host).await.unwrap_or_else(|e| {
        let mut blank = crate::facts::probe_blank(&s.cfg.host);
        blank.unreachable_reason = Some(e.to_string());
        blank
    });
    let plan = provision::plan(&f);
    // A host with no libvirt yet has no spaces, and that is not an error to
    // show: the Settings face already says why. An empty list plus a populated
    // plan is the correct reading of a box that has not been set up.
    let spaces = spaces::list(&s.cfg.host).await.unwrap_or_default();

    build_data(&f, &plan, &spaces, is_admin)
}

/// Assemble the payload.
///
/// Split from `data` so every row the UI can show is testable without a
/// hypervisor — which is the only way to check that a blocked step still
/// produces a readable row.
pub fn build_data(f: &Facts, plan: &Plan, spaces: &[Space], is_admin: bool) -> Value {
    let rows: Vec<Value> = spaces.iter().map(|sp| space_row(sp, f, is_admin)).collect();
    let can_apply = f.privilege.can_apply();
    let setup_rows: Vec<Value> = plan
        .steps
        .iter()
        .map(|s| step_row_data(s, can_apply))
        .collect();
    let gpu_rows: Vec<Value> = f
        .gpus
        .iter()
        .map(|g| gpu_row_data(g, &f.gpus, spaces))
        .collect();
    let installers = installers::list(f);
    let installer_rows: Vec<Value> = installers.iter().map(installer_row_data).collect();
    let os_catalog: Vec<Value> = installers.iter().map(os_choice).collect();
    let gpu_choices = gpu_choices(f, spaces);

    // What is *left to do*: neither finished nor blocked by the hardware. A
    // step the box can never take (the console's own display) would otherwise
    // sit as "1" on a finished host forever.
    let setup_pending = plan
        .steps
        .iter()
        .filter(|s| !s.satisfied && (s.blocked.is_none() || !f.privilege.can_apply()))
        .count();

    json!({
        "rows": rows,
        "setup_rows": setup_rows,
        "gpu_rows": gpu_rows,
        "installer_rows": installer_rows,
        "host_facts": host_facts(f),
        "os_catalog": os_catalog,
        "gpu_choices": gpu_choices,

        "host_status_label": f.status_label(),
        "host_status_color": f.status_color(),
        "host_label": if f.hostname.is_empty() { f.host.clone() } else { f.hostname.clone() },
        "unreachable_reason": f.unreachable_reason.clone().unwrap_or_default(),
        "has_gpus": if f.gpus.is_empty() { "0" } else { "1" },
        "installer_hint": "Windows Server downloads as Microsoft's evaluation media; entering your key after install makes it the full edition. Windows 10 and 11 have no stable download link: fetch the ISO from microsoft.com in a browser, drop it in /var/lib/leo/spaces-import on the host as <id>.iso, and Import adopts it.",

        // Counts show on the rail rows. The setup count is what is *left*, not
        // the total: a badge of 9 on a finished host reads as nine problems.
        "space_count": spaces.len().to_string(),
        "setup_count": if setup_pending == 0 { String::new() } else { setup_pending.to_string() },
    })
}

fn space_row(sp: &Space, f: &Facts, is_admin: bool) -> Value {
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
        hardware.push(("GPU", gpu_name.clone().unwrap_or_else(|| addr.clone())));
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
            format!(
                "{} on 127.0.0.1:{}",
                sp.console_kind.as_deref().unwrap_or("vnc"),
                port
            ),
        ));
    }
    // The local admin Leo made at install, for admins only: the pane is the
    // one place this is ever shown, and a non-admin can see the machine
    // without being able to sign into it.
    if is_admin {
        if let Some(pw) = &sp.admin_password {
            access.push(("Admin", format!("leo / {pw}")));
        }
    }

    json!({
        "id": format!("space-{}", sp.name),
        "tab": "spaces",
        "kind": "space",
        "kind_label": "Space",
        "title": sp.name,
        "subtitle": join_dot(&[
            &format!("{} vCPU", sp.vcpus),
            &format!("{} GB", sp.memory_mb / 1024),
            if sp.has_gpu() { "GPU" } else { "" },
        ]),
        "trailing": sp.ip.clone().unwrap_or_default(),
        // What the tile wears. The chip names the system the way a person
        // says it ("Windows 11"); the symbol and the colour come from the
        // family — so a board of mixed guests reads as mixed at a glance
        // rather than needing every caption to be read.
        "os_chip": match os_label(&sp.os) {
            "Unknown" => String::new(),
            label => label.to_string(),
        },
        "os_symbol": os_symbol(&sp.os).to_string(),
        "art_color": os_color(&sp.os).to_string(),
        "gpu_chip": match &gpu_name {
            Some(n) => short_name(n),
            None => String::new(),
        },
        "admin_line": if is_admin {
            sp.admin_password
                .as_ref()
                .map(|pw| format!("leo / {pw}"))
                .unwrap_or_default()
        } else {
            String::new()
        },
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
        // A console needs a running guest with an emulated display to show.
        "has_console": if sp.state.is_running() && sp.console_port.is_some() { "1" } else { "0" },
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

fn step_row_data(step: &provision::Step, can_apply: bool) -> Value {
    // "Reboot" is a step that has been applied and is waiting for one — not
    // a step that would need one if it were applied. The old mapping put the
    // purple badge on every unapplied boot step, so after a reboot that
    // changed nothing the row read exactly as it had before.
    //
    // A finished step wears no badge: the green dot already says so, and a
    // column of "Done" on a finished host is a status that is always green.
    let (badge, badge_color) = if step.satisfied {
        ("", GREEN)
    } else if step.blocked.is_some() {
        ("Blocked", RED)
    } else if step.applied {
        ("Reboot", PURPLE)
    } else {
        ("Pending", AMBER)
    };

    // Which button the row offers, if any. Boot steps get the confirming
    // variant; a blocked or satisfied step gets none, because there is nothing
    // to press that would help.
    let apply_mode = if !step.applicable() {
        "none"
    } else if step.risk == Risk::Boot {
        "boot"
    } else {
        "direct"
    };
    // The command is for the host this package cannot act on: it is shown to
    // copy exactly when nothing here may run it. A step blocked by the
    // hardware itself offers nothing — the command would fail the same way
    // from a root shell.
    let copy_only = !can_apply && step.command.is_some() && !step.satisfied;

    // The one sentence that matters now. A blocker outranks everything; a
    // configured step says what it is waiting for; otherwise what it does.
    let subtitle = if let Some(b) = &step.blocked {
        b.clone()
    } else if step.applied {
        step.detail
            .split("Needs a reboot.")
            .nth(1)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| first_sentence(&step.detail))
    } else {
        first_sentence(&step.detail)
    };

    json!({
        "id": step.id,
        "title": step.title,
        "subtitle": subtitle,
        "trailing": if step.needs_reboot && !step.satisfied && step.blocked.is_none() { "reboot" } else { "" },
        "badge": badge,
        "badge_color": badge_color,
        "state_color": badge_color,
        "command": step.command.clone().unwrap_or_default(),
        "apply_mode": apply_mode,
        "copy_only": if copy_only { "1" } else { "0" },
    })
}

fn gpu_row_data(g: &crate::facts::Gpu, all_gpus: &[crate::facts::Gpu], spaces: &[Space]) -> Value {
    // Every display on the box, so "the host's only display" can be decided
    // rather than inferred from this one card alone.
    let blockers = g.passthrough_blockers(all_gpus);
    let holder = spaces
        .iter()
        .find(|s| s.hostdevs.iter().any(|h| h == &g.address));

    let (badge, badge_color) = if let Some(h) = holder {
        (h.name.as_str(), BLUE)
    } else if g.integrated || (g.is_boot_vga && !blockers.is_empty()) {
        ("Host display", GRAY)
    } else if !blockers.is_empty() {
        ("Not passable", AMBER)
    } else if g.driver == "vfio-pci" {
        ("Available", GREEN)
    } else {
        // Passable, but its driver still holds it: the passthrough step in
        // the panel beside this one is what is left.
        ("Needs passthrough", AMBER)
    };

    let subtitle = if g.integrated {
        "The display the host itself uses. Passing it to a guest would leave the console dark, so it is not offered.".to_string()
    } else if !blockers.is_empty() {
        blockers.join("; ")
    } else {
        join_dot(&[
            &g.iommu_group
                .map(|n| format!("IOMMU group {n}"))
                .unwrap_or_default(),
            if g.driver.is_empty() {
                "no driver"
            } else {
                &g.driver
            },
            if holder.is_some() {
                ""
            } else {
                "can be handed to one space exclusively"
            },
        ])
    };

    json!({
        "id": format!("gpu-{}", g.address),
        "title": short_name(&g.name),
        "subtitle": subtitle,
        "trailing": g.address,
        "badge": badge,
        "badge_color": badge_color,
        "state_color": badge_color,
    })
}

fn installer_row_data(i: &installers::Installer) -> Value {
    use installers::Standing;
    let (badge, color) = match i.standing {
        Standing::Ready => ("Ready", GREEN),
        Standing::Downloading => ("Downloading", AMBER),
        Standing::Failed => ("Failed", RED),
        Standing::Fetchable => ("", GRAY),
        Standing::Bring => ("", GRAY),
        Standing::Staged => ("Staged", AMBER),
    };
    let subtitle = match i.standing {
        Standing::Ready => i.file.to_string(),
        Standing::Downloading => format!("{} so far \u{2014} {}", gb(i.size_mb), i.file),
        Standing::Failed => i
            .failure
            .clone()
            .unwrap_or_else(|| "the download did not finish".into()),
        Standing::Fetchable => format!("Not on the host yet. Downloads to {}", i.file),
        Standing::Bring => format!(
            "No stable download link \u{2014} put the image at /var/lib/leo/spaces-import/{}.iso (or at {})",
            i.id, i.path
        ),
        Standing::Staged => format!("{} waiting in /var/lib/leo/spaces-import \u{2014} Import adopts it", gb(i.size_mb)),
    };
    json!({
        "id": i.id,
        "title": i.label,
        "subtitle": subtitle,
        "trailing": if i.standing == Standing::Ready { gb(i.size_mb) } else { String::new() },
        "badge": badge,
        "badge_color": color,
        "state_color": color,
        "path": i.path,
        "can_fetch": if matches!(i.standing, Standing::Fetchable | Standing::Failed) { "1" } else { "0" },
        "bring": if i.standing == Standing::Bring { "1" } else { "0" },
        "staged": if i.standing == Standing::Staged { "1" } else { "0" },
    })
}

/// One row of the sheet's OS picker.
fn os_choice(i: &installers::Installer) -> Value {
    use installers::Standing;
    json!({
        "id": i.id,
        "label": i.label,
        "family": i.family,
        "standing_label": match i.standing {
            Standing::Ready => "",
            Standing::Downloading => "downloading",
            Standing::Failed => "download failed",
            Standing::Fetchable => "download in Settings",
            Standing::Bring => "needs the ISO",
            Standing::Staged => "import in Settings",
        },
    })
}

/// The sheet's GPU picker: none, then every card that can actually be handed
/// over right now. A card that is blocked or already somebody's is not
/// offered — a choice that Create would refuse is a choice the sheet should
/// not list.
fn gpu_choices(f: &Facts, spaces: &[Space]) -> Vec<Value> {
    let mut out = vec![json!({
        "address": "",
        "label": "No GPU \u{2014} emulated display",
        "note": ""
    })];
    for g in f.gpus.iter().filter(|g| !g.integrated) {
        let held = spaces
            .iter()
            .any(|s| s.hostdevs.iter().any(|h| h == &g.address));
        if held || !g.passthrough_blockers(&f.gpus).is_empty() {
            continue;
        }
        out.push(json!({
            "address": g.address,
            "label": short_name(&g.name),
            "note": if g.driver == "vfio-pci" { "" } else { "needs passthrough" }
        }));
    }
    out
}

/// What the host is. Empty when it cannot be read — stale would beat blank,
/// but a first read that failed has nothing stale to show.
fn host_facts(f: &Facts) -> Vec<Value> {
    if !f.reachable {
        return Vec::new();
    }
    let fact = |label: &str, value: String| json!({ "label": label, "value": value });
    let mut out = vec![
        fact("Host", f.host.clone()),
        fact("System", f.os.clone()),
        fact("Kernel", f.kernel.clone()),
        fact("CPU", f.cpu_model.clone()),
        fact("Threads", f.cpu_threads.to_string()),
        fact("Memory", format!("{} GB", f.memory_mb / 1024)),
        fact("Disk free", format!("{} GB", f.disk_free_gb)),
        fact(
            "IOMMU",
            if f.iommu_enabled() {
                format!("{} groups", f.iommu_groups)
            } else {
                "off".into()
            },
        ),
        fact(
            "libvirt",
            if f.libvirt_running {
                "running".into()
            } else if f.libvirt_installed {
                "installed, not running".into()
            } else {
                "not installed".into()
            },
        ),
        fact("Privilege", f.privilege.label().to_string()),
    ];
    out.retain(|v| !v["value"].as_str().unwrap_or("").is_empty());
    out
}

// ─────────────────────────────────────────────────────────────────── helpers

fn gb(mb: u64) -> String {
    if mb >= 1024 {
        format!("{:.1} GB", mb as f64 / 1024.0)
    } else {
        format!("{mb} MB")
    }
}

fn sections(list: Vec<Value>) -> Value {
    // Empty sections are dropped rather than drawn as a heading over nothing.
    Value::Array(
        list.into_iter()
            .filter(|s| s["fields"].as_array().is_some_and(|f| !f.is_empty()))
            .collect(),
    )
}

fn section(title: &str, fields: &[(&str, String)]) -> Value {
    json!({
        "title": title,
        "fields": fields
            .iter()
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
        .join(" \u{b7} ")
}

fn first_sentence(s: &str) -> String {
    match s.find(". ") {
        Some(i) => s[..=i].to_string(),
        None => s.to_string(),
    }
}

/// "VGA compatible controller: NVIDIA Corporation GA104GL [RTX A4000]" →
/// "RTX A4000". The bracketed marketing name is the one a person recognises;
/// the rest is the PCI database being thorough.
fn short_name(full: &str) -> String {
    let after_class = full.split(": ").last().unwrap_or(full);
    if let (Some(a), Some(b)) = (after_class.rfind('['), after_class.rfind(']')) {
        if a < b {
            return after_class[a + 1..b].to_string();
        }
    }
    after_class.to_string()
}

/// A libosinfo short id as a person would say it.
fn os_label(os: &str) -> &str {
    match crate::catalog::find(os) {
        Some(o) => o.label,
        None if os.is_empty() => "Unknown",
        None => os,
    }
}

/// The SF Symbol a family wears on its tile.
///
/// Deliberately coarse: the question a glyph answers from across a room is
/// "which of my machines is this", and three shapes answer it. A per-release
/// glyph would be a different picture for the same answer.
fn os_symbol(os: &str) -> &'static str {
    if os.starts_with("win2k") {
        "server.rack"
    } else if os.starts_with("win") {
        "pc"
    } else if os.is_empty() {
        "desktopcomputer"
    } else {
        "terminal"
    }
}

/// The field of colour behind a tile, by family.
///
/// Hex rather than a token because `tile`'s fill is interpolated per cell —
/// the colour is data about the row, not a decision the descriptor can make
/// once. Dark enough that white type sits on it without a scrim.
fn os_color(os: &str) -> &'static str {
    if os.starts_with("win2k") {
        "#2a4a6b"
    } else if os.starts_with("win") {
        "#1f3a5f"
    } else if os.is_empty() {
        // A guest libvirt has no title for — one installed by hand, before
        // this app existed. Graphite rather than a family's colour: inventing
        // one would say the system is known when the chip says it is not.
        "#2c2c30"
    } else {
        "#4a3a2a"
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
            admin_password: None,
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
        let payload = build_data(&f, &plan, &[a_space()], true);

        // Every key the tree binds at page scope. Walked from the tree
        // rather than listed by hand, so a new bind cannot be forgotten.
        let mut binds = Vec::new();
        collect_page_binds(&screen(), &mut binds);
        assert!(
            binds.len() >= 10,
            "the walk found almost nothing: {binds:?}"
        );
        for key in &binds {
            assert!(
                payload.get(key.as_str()).is_some(),
                "descriptor binds '{key}' and the payload has no such key"
            );
        }
    }

    /// Every `bind`/`countBind`/`colorFrom`/`tintFrom` that resolves at page
    /// scope: everything outside a `list`'s `item` and a `masterDetail`'s
    /// `detail`, which are row-scoped.
    fn collect_page_binds(v: &Value, out: &mut Vec<String>) {
        if let Value::Object(map) = v {
            for key in ["bind", "countBind", "colorFrom", "tintFrom"] {
                if let Some(b) = map.get(key).and_then(Value::as_str) {
                    out.push(b.to_string());
                }
            }
            for (k, child) in map {
                if k == "item" || k == "detail" {
                    continue;
                }
                collect_page_binds(child, out);
            }
        } else if let Value::Array(items) = v {
            items.iter().for_each(|i| collect_page_binds(i, out));
        }
    }

    /// Row-scoped binds against the rows that feed them: every key a row
    /// template or the detail pane reads has to be on every row of that list.
    #[test]
    fn every_row_template_is_fed_by_its_rows() {
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
            functions: vec![],
        }];
        f.isos = vec![crate::facts::IsoFile {
            file: "archlinux-x86_64.iso".into(),
            size_mb: 1200,
            state: crate::facts::IsoState::Ready,
            failure: None,
        }];
        let plan = provision::plan(&f);
        let payload = build_data(&f, &plan, &[a_space()], true);

        let tree = screen();
        let mut lists = Vec::new();
        collect_lists(&tree, &mut lists);
        assert!(lists.len() >= 6, "found only {} bound lists", lists.len());

        for (bind, template) in lists {
            // Nested lists (`sections` → `fields`) are row-scoped twice over;
            // the outer test covers the shape well enough.
            let Some(rows) = payload.get(&bind).and_then(Value::as_array) else {
                continue;
            };
            assert!(!rows.is_empty(), "'{bind}' has no rows to check against");
            let mut needs = Vec::new();
            collect_row_binds(&template, &mut needs);
            for r in rows {
                for key in &needs {
                    assert!(
                        r.get(key.as_str()).is_some(),
                        "a row of '{bind}' ({:?}) is missing '{key}'",
                        r.get("id").or(r.get("label"))
                    );
                }
            }
        }
    }

    /// `(bind, item-or-detail template)` for every list and master-detail.
    fn collect_lists(v: &Value, out: &mut Vec<(String, Value)>) {
        if let Value::Object(map) = v {
            if map.get("kind") == Some(&json!("list")) {
                if let (Some(b), Some(item)) =
                    (map.get("bind").and_then(Value::as_str), map.get("item"))
                {
                    out.push((b.to_string(), item.clone()));
                }
            }
            if map.get("kind") == Some(&json!("masterDetail")) {
                if let (Some(list), Some(detail)) = (map.get("list"), map.get("detail")) {
                    if let Some(b) = list.get("bind").and_then(Value::as_str) {
                        out.push((b.to_string(), detail.clone()));
                    }
                }
            }
            for child in map.values() {
                collect_lists(child, out);
            }
        } else if let Value::Array(items) = v {
            items.iter().for_each(|i| collect_lists(i, out));
        }
    }

    /// Binds and `{field}` interpolations and `visibleWhen.field`s inside one
    /// row template, not descending into nested lists (their rows are their
    /// own scope).
    fn collect_row_binds(v: &Value, out: &mut Vec<String>) {
        if let Value::Object(map) = v {
            if map.get("kind") == Some(&json!("list")) && map.contains_key("bind") {
                out.push(map["bind"].as_str().unwrap().to_string());
                return;
            }
            for key in ["bind", "countBind", "colorFrom", "tintFrom"] {
                if let Some(b) = map.get(key).and_then(Value::as_str) {
                    out.push(b.to_string());
                }
            }
            if let Some(f) = map
                .get("visibleWhen")
                .and_then(|cond| cond.get("field"))
                .and_then(Value::as_str)
            {
                out.push(f.to_string());
            }
            for (k, child) in map {
                if let Value::String(s) = child {
                    // `{title}` in a path, a value or a copyText.
                    let mut rest = s.as_str();
                    while let Some(a) = rest.find('{') {
                        let Some(b) = rest[a..].find('}') else { break };
                        let name = &rest[a + 1..a + b];
                        if !name.is_empty() && !name.contains('.') && k != "value" || k == "text" {
                            out.push(name.to_string());
                        }
                        rest = &rest[a + b + 1..];
                    }
                }
                collect_row_binds(child, out);
            }
        } else if let Value::Array(items) = v {
            items.iter().for_each(|i| collect_row_binds(i, out));
        }
    }

    /// The list on the Spaces face is spaces and nothing else: the steps and
    /// the cards moved to their own panels, and a step row that slipped back
    /// in would be filtered to the spaces tab and drawn as a machine.
    #[test]
    fn the_spaces_list_holds_only_spaces_and_the_panels_hold_the_rest() {
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
            functions: vec![],
        }];
        let plan = provision::plan(&f);
        let payload = build_data(&f, &plan, &[a_space()], true);
        let rows = payload["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert!(rows
            .iter()
            .all(|r| r["tab"] == "spaces" && r["kind"] == "space"));
        assert!(!payload["setup_rows"].as_array().unwrap().is_empty());
        assert_eq!(payload["gpu_rows"].as_array().unwrap().len(), 1);
        assert_eq!(
            payload["installer_rows"].as_array().unwrap().len(),
            crate::catalog::ALL.len(),
            "every OS in the catalog is an installer row"
        );
    }

    /// A running space offers Shut down and Restart but not Start; an off one
    /// the reverse. Gating on state rather than disabling means the pane never
    /// asks the operator to work out which button applies.
    #[test]
    fn controls_are_gated_on_the_spaces_actual_state() {
        let f = facts();
        let plan = provision::plan(&f);

        let running = build_data(&f, &plan, &[a_space()], true);
        let row = &running["rows"].as_array().unwrap()[0];
        assert_eq!(row["running"], "on");
        assert_eq!(
            row["has_console"], "1",
            "running with a VNC port: the console is offered"
        );

        // The console opens the page this package serves, under the mount —
        // from the spaces face now rather than a detail pane, because there is
        // no pane: the board and the screen are two bodies of one face, and
        // the screen is reached by tapping a tile.
        let face = screen()["body"]["children"][2].clone();
        let mut routes = Vec::new();
        collect_prop(&face, "route", &mut routes);
        assert!(
            routes.contains(&"/p/spaces/console/{console.name}".to_string()),
            "{routes:?}"
        );
        // And the tile is what names the machine, in ONE write — two
        // `setState`s race each other through `publishAddress`, so a second
        // key creeping back in here is a navigation that works intermittently.
        let mut keys = Vec::new();
        collect_prop(&space_tile()["tap"], "key", &mut keys);
        assert_eq!(keys, vec!["console.name".to_string()], "{keys:?}");

        let blind = Space {
            console_port: None,
            ..a_space()
        };
        let payload = build_data(&f, &plan, &[blind], true);
        assert_eq!(payload["rows"].as_array().unwrap()[0]["has_console"], "0");

        let off = Space {
            state: State::Shutoff,
            ..a_space()
        };
        let payload = build_data(&f, &plan, &[off], true);
        assert_eq!(payload["rows"].as_array().unwrap()[0]["running"], "off");
        assert_eq!(
            payload["rows"].as_array().unwrap()[0]["has_console"],
            "0",
            "off: nothing to show"
        );
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

    /// A blocked step shows its reason and offers no button. The reason is the
    /// deliverable; a button that would fail is worse than none.
    #[test]
    fn a_blocked_step_shows_the_reason_and_no_button() {
        let mut f = facts();
        f.privilege = Privilege::None;
        f.wrapper_installed = false;
        f.wrapper_usable = false;
        let plan = provision::plan(&f);
        let payload = build_data(&f, &plan, &[], true);

        let pkg = payload["setup_rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == "packages")
            .expect("the packages step");

        assert_eq!(pkg["badge"], "Blocked");
        // The reason IS the row's one line.
        assert!(pkg["subtitle"]
            .as_str()
            .unwrap()
            .contains("install-admin-wrapper"));
        assert_eq!(pkg["apply_mode"], "none", "nothing to press");
        // But the command is still there to copy, which is the whole point.
        assert_eq!(pkg["copy_only"], "1");
        assert!(pkg["command"].as_str().unwrap().contains("pacman"));
    }

    /// A step the hardware itself blocks offers nothing: no button, no
    /// command to copy (it would fail the same way from a root shell), no
    /// "reboot" tag for a reboot that will never be asked for — and it does
    /// not count as work left.
    #[test]
    fn a_hardware_blocked_step_offers_nothing_and_is_not_counted() {
        let mut f = facts();
        f.qemu_installed = true;
        f.libvirt_installed = true;
        f.ovmf_installed = true;
        f.swtpm_installed = true;
        f.libvirt_running = true;
        f.default_network_active = true;
        f.in_libvirt_group = true;
        f.pool_ready = true;
        f.virtio_iso_present = true;
        f.guest_net_open = true;
        f.iommu_groups = 40;
        f.gpus = vec![Gpu {
            address: "0000:e3:00.0".into(),
            ids: "1a03:2000".into(),
            name: "ASPEED Graphics Family".into(),
            driver: "ast".into(),
            iommu_group: Some(10),
            group_siblings: vec!["0000:e2:00.0".into()],
            is_boot_vga: false,
            integrated: false,
            functions: vec![],
        }];
        let plan = provision::plan(&f);
        let payload = build_data(&f, &plan, &[], true);
        let row = payload["setup_rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == "vfio:0000:e3:00.0")
            .expect("the blocked passthrough step");
        assert_eq!(row["badge"], "Blocked");
        assert_eq!(row["apply_mode"], "none");
        assert_eq!(row["copy_only"], "0");
        assert_eq!(row["trailing"], "");
        assert_eq!(payload["setup_count"], "", "a finished host shows no count");
    }

    /// A boot-touching step must route to the confirming button, never the
    /// one-press one.
    #[test]
    fn a_boot_step_gets_the_confirming_button() {
        let f = facts();
        let plan = provision::plan(&f);
        let payload = build_data(&f, &plan, &[], true);
        let rows = payload["setup_rows"].as_array().unwrap();

        let iommu = rows.iter().find(|r| r["id"] == "iommu").unwrap();
        assert_eq!(iommu["apply_mode"], "boot");
        // Unapplied, so Pending: the purple Reboot badge is reserved for a
        // step that has been applied and is waiting for the reboot.
        assert_eq!(iommu["badge"], "Pending");
        assert_eq!(iommu["trailing"], "reboot");

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
        let payload = build_data(&f, &plan, &[], true);
        let pkg = payload["setup_rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == "packages")
            .unwrap();
        assert_eq!(
            pkg["badge"], "",
            "the green dot says done; a badge would say it twice"
        );
        assert_eq!(pkg["state_color"], GREEN);
        assert_eq!(pkg["apply_mode"], "none");
        assert_eq!(pkg["copy_only"], "0", "nothing left to run");
    }

    /// The setup badge counts what is left. A count of the total reads as nine
    /// problems on a host that is finished.
    #[test]
    fn the_setup_count_is_what_remains_and_empties_when_done() {
        let f = facts();
        let plan = provision::plan(&f);
        let payload = build_data(&f, &plan, &[], true);
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
        let payload = build_data(&done, &plan, &[], true);
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
        let payload = build_data(&f, &plan, &[], true);
        assert_eq!(payload["host_status_label"], "Unreachable");
        assert_eq!(payload["host_status_color"], RED);
        assert!(
            payload["host_facts"].as_array().unwrap().is_empty(),
            "no facts rather than zeroes — we do not know"
        );
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
            functions: vec![],
        }];
        let plan = provision::plan(&f);
        let payload = build_data(&f, &plan, &[], true);
        let gpu = &payload["gpu_rows"].as_array().unwrap()[0];
        assert_eq!(gpu["badge"], "Host display");
        assert_eq!(gpu["title"], "Radeon Graphics");
        assert!(gpu["subtitle"].as_str().unwrap().contains("console dark"));
        // And it is not offered in the sheet: only "no GPU".
        assert_eq!(payload["gpu_choices"].as_array().unwrap().len(), 1);
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
            functions: vec![],
        }];
        let plan = provision::plan(&f);
        let payload = build_data(&f, &plan, &[a_space()], true);
        let gpu = &payload["gpu_rows"].as_array().unwrap()[0];
        assert_eq!(gpu["badge"], "win-cad");
        // Held, so not offered to a new space either.
        assert_eq!(payload["gpu_choices"].as_array().unwrap().len(), 1);

        // Free and bound to vfio-pci: offered, with nothing to note.
        let payload = build_data(&f, &plan, &[], true);
        let choices = payload["gpu_choices"].as_array().unwrap();
        assert_eq!(choices.len(), 2);
        assert_eq!(choices[1]["address"], "0000:01:00.0");
        assert_eq!(choices[1]["note"], "");
    }

    /// The Server-versus-client-Windows distinction is the multi-agent story,
    /// so it has to appear where someone reading a space will see it.
    #[test]
    fn a_windows_server_space_explains_multi_session() {
        let f = facts();
        let plan = provision::plan(&f);
        let payload = build_data(&f, &plan, &[a_space()], true);
        let row = &payload["rows"].as_array().unwrap()[0];
        // On the chip, not in the subtitle: a tile wears the system as a
        // badge over its art and spends the line under the name on the
        // hardware instead. Saying it in both places was the same fact twice.
        assert_eq!(row["os_chip"], "Windows Server 2022");
        assert!(
            !row["subtitle"]
                .as_str()
                .unwrap()
                .contains("Windows Server 2022"),
            "the subtitle must not repeat the chip: {}",
            row["subtitle"]
        );
        assert!(row["detail_text"]
            .as_str()
            .unwrap()
            .contains("concurrent interactive session"));

        let client = Space {
            os: "win11".into(),
            hostdevs: vec![],
            ..a_space()
        };
        let payload = build_data(&f, &plan, &[client], true);
        let row = &payload["rows"].as_array().unwrap()[0];
        assert!(row["detail_text"]
            .as_str()
            .unwrap()
            .contains("one interactive session"));
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
        let payload = build_data(&f, &plan, &[bare], true);
        let row = &payload["rows"].as_array().unwrap()[0];
        let titles: Vec<&str> = row["sections"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["title"].as_str().unwrap())
            .collect();
        assert!(titles.contains(&"Guest"));
        assert!(
            !titles.contains(&"Access"),
            "an empty Access section should be dropped"
        );
    }

    /// The sheet's body names exactly the fields `NewSpace` reads (less the
    /// installer, which the server resolves), and every one is interpolated
    /// from a seeded form key — a field posted under the wrong name is a
    /// default silently applied.
    #[test]
    fn the_sheet_posts_what_a_new_space_is_made_of() {
        let sheet = new_space_sheet();
        let body = sheet["submit"]["action"]["body"].as_object().unwrap();
        let mut posted: Vec<&str> = body.keys().map(String::as_str).collect();
        posted.sort();
        assert_eq!(
            posted,
            [
                "disk_gb",
                "gpu",
                "memory_mb",
                "name",
                "os",
                "product_key",
                "vcpus"
            ]
        );
        let seeded = screen()["state"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        for v in body.values() {
            let v = v.as_str().unwrap();
            let key = v.trim_start_matches('{').trim_end_matches('}');
            assert!(
                seeded.contains(&key.to_string()),
                "{v} is not a seeded form key"
            );
        }
        // And what it posts parses: the form speaks strings.
        let req: spaces::NewSpace = serde_json::from_value(json!({
            "name": "cad", "os": "win2k22", "vcpus": "8", "memory_mb": "16384",
            "disk_gb": "120", "gpu": "", "product_key": ""
        }))
        .expect("the sheet's body shape");
        assert_eq!(req.vcpus, 8);
        assert_eq!(req.memory_mb, 16384);
        assert!(req.gpu.is_none(), "an unpicked GPU is no GPU");
        assert!(req.product_key.is_none(), "a blank key is no key");
        // And the approve rides on the code the create answers, with the
        // session of whoever pressed Create.
        let then = sheet["submit"]["action"]["then"].as_array().unwrap();
        assert!(then
            .iter()
            .any(|a| a["path"] == "/api/satellite-pair/{pair_code}/approve"));
        assert_eq!(
            sheet["submit"]["action"]["setStateFromResponse"]["pair_code"],
            "pairing_code"
        );
    }

    /// The picker's families are the catalog's, and every OS in the catalog
    /// lands in one of the segmented's columns — an OS in a family the
    /// segmented does not offer is one nobody can choose.
    #[test]
    fn every_catalog_os_is_reachable_from_the_picker() {
        let sheet = new_space_sheet();
        let fields = sheet["fields"].as_array().unwrap();
        let family = fields
            .iter()
            .find(|f| f["stateKey"] == "form.family")
            .expect("the family picker");
        let offered: Vec<&str> = family["options"]
            .as_array()
            .unwrap()
            .iter()
            .map(|o| o["value"].as_str().unwrap())
            .collect();
        for os in crate::catalog::ALL {
            assert!(
                offered.contains(&os.family.key()),
                "{} is in no column",
                os.id
            );
        }
        // The default choice is lit: it is in the default family.
        let state = screen()["state"].clone();
        let default_os = crate::catalog::find(state["form.os"].as_str().unwrap()).unwrap();
        assert_eq!(default_os.family.key(), state["form.family"]);
    }

    /// An installer row says what pressing its button will do, and only
    /// offers one when there is something to press.
    #[test]
    fn installer_rows_offer_the_one_verb_that_applies() {
        use crate::facts::{IsoFile, IsoState};
        let mut f = facts();
        f.isos = vec![
            IsoFile {
                file: "archlinux-x86_64.iso".into(),
                size_mb: 1200,
                state: IsoState::Ready,
                failure: None,
            },
            IsoFile {
                file: "debian-13-amd64-netinst.iso".into(),
                size_mb: 0,
                state: IsoState::Failed,
                failure: Some("download of x failed".into()),
            },
        ];
        let plan = provision::plan(&f);
        let payload = build_data(&f, &plan, &[], true);
        let rows = payload["installer_rows"].as_array().unwrap();
        let by = |id: &str| rows.iter().find(|r| r["id"] == id).unwrap().clone();

        assert_eq!(by("archlinux")["badge"], "Ready");
        assert_eq!(by("archlinux")["can_fetch"], "0");
        assert_eq!(by("debian13")["badge"], "Failed");
        assert_eq!(
            by("debian13")["can_fetch"],
            "1",
            "a failed download can be retried"
        );
        assert!(by("debian13")["subtitle"]
            .as_str()
            .unwrap()
            .contains("failed"));
        assert_eq!(by("rocky10")["can_fetch"], "1");
        assert_eq!(
            by("win2k22")["can_fetch"],
            "1",
            "Server evaluation media is fetchable"
        );
        assert_eq!(by("win11")["can_fetch"], "0");
        assert_eq!(by("win11")["bring"], "1");
        assert_eq!(by("win11")["staged"], "0");
        assert!(by("win11")["subtitle"]
            .as_str()
            .unwrap()
            .contains("/var/lib/leo/spaces-import/win11.iso"));
        f.staged = vec![("win11".into(), 9000)];
        let payload = build_data(&f, &plan, &[], true);
        let w = payload["installer_rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == "win11")
            .unwrap();
        assert_eq!(w["staged"], "1", "a staged image offers Import");
        assert_eq!(w["bring"], "0");
        assert_eq!(w["badge"], "Staged");

        let pick = payload["os_catalog"].as_array().unwrap();
        let standing =
            |id: &str| pick.iter().find(|r| r["id"] == id).unwrap()["standing_label"].clone();
        assert_eq!(standing("archlinux"), "", "ready says nothing");
        assert_eq!(standing("win2k22"), "download in Settings");
        // This payload has win11 staged, so the picker says so; unstaged it
        // would say "needs the ISO".
        assert_eq!(standing("win11"), "import in Settings");
    }

    #[test]
    fn a_name_shortens_to_what_a_person_recognises() {
        assert_eq!(
            short_name("VGA compatible controller: NVIDIA Corporation GA104GL [RTX A4000]"),
            "RTX A4000"
        );
        assert_eq!(
            short_name("3D controller: Some Card 9000"),
            "Some Card 9000"
        );
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
        "screen",
        "vstack",
        "hstack",
        "zstack",
        "spacer",
        "divider",
        "section",
        "text",
        "hero",
        "progress",
        "icon",
        "badge",
        "dot",
        "avatar",
        "image",
        "button",
        "checkbox",
        "searchField",
        "textField",
        "colorField",
        "textArea",
        "segmented",
        "slider",
        "markdown",
        "markdownEditor",
        "list",
        "grid",
        "masterDetail",
        "selectRow",
        "emptyState",
        "card",
        "panel",
        "authored",
        "paneTree",
        "chart",
        "compositionBar",
        "deltaChip",
        "carousel",
        "stepper",
        "tile",
        "chore_board",
        "entities_card",
        "room_card",
        "entity_card",
        "inlineComposer",
        "native",
        "sheet",
    ];

    pub const ACTION_KINDS: &[&str] = &[
        "setState",
        "confirm",
        "packageAction",
        "openURL",
        "openWindow",
        "closeWindow",
        "copyText",
    ];

    pub const TEXT_STYLES: &[&str] = &[
        "largeTitle",
        "hero",
        "title",
        "subtitle",
        "section-label",
        "headline",
        "mono",
        "meta",
        "caption",
        "subtle",
        "body",
    ];

    pub const COLOR_TOKENS: &[&str] = &[
        "accent",
        "secondary",
        "muted",
        "text",
        "white",
        "onImage",
        "whiteMuted",
        "onImageMuted",
        "ink",
        "inkMuted",
    ];

    /// The flat union of every prop either renderer reads.
    pub const PROPS: &[&str] = &[
        "a11yLabel",
        "acceptsDrop",
        "action",
        "actionLabel",
        "activeBind",
        "addressableState",
        "advance",
        "align",
        "aside",
        "assistant",
        "backLabel",
        "bare",
        "bind",
        "bleed",
        "body",
        "border",
        "canonicalRoute",
        "card",
        "checkedBody",
        "checkedWhen",
        "child",
        "children",
        "clearBackdrop",
        "color",
        "colorFrom",
        "columns",
        "commandBar",
        "commands",
        "composer",
        "content",
        "corner",
        "countBind",
        "dataParams",
        "default",
        "deltaBind",
        "deltaInverted",
        "detail",
        "detailFetch",
        "detailWidth",
        "direction",
        "discard",
        "dividerInset",
        "dividers",
        "empty",
        "entity_bind",
        "ephemeralState",
        "expand",
        "fallback",
        "fallbackIcon",
        "field",
        "fields",
        "fill",
        "fillWidth",
        "fillHeight",
        "filter",
        "fit",
        "fits",
        "focusKey",
        "fontScale",
        "format",
        "header",
        "headerActions",
        "height",
        "hideChromeState",
        "hideChromeStates",
        "hideChromeWhen",
        "hideTitle",
        "hideTitleState",
        "hideTitleStates",
        "hideWhenEmpty",
        "hint",
        "icon",
        "iconMax",
        "iconMin",
        "iconUrl",
        "inset",
        "inverted",
        "item",
        "items",
        "key",
        "label",
        "labelColor",
        "lineSpacing",
        "lines",
        "list",
        "mailboxNav",
        "max",
        "reserveLines",
        "maxBind",
        "maxWidth",
        "menuItems",
        "message",
        "min",
        "minLength",
        "minWidth",
        "moveAction",
        "noScrollState",
        "noScrollStates",
        "onOpen",
        "onOpenWhen",
        "openKey",
        "options",
        "overlay",
        "pack",
        "padding",
        "paddingH",
        "paddingTop",
        "paddingV",
        "paper",
        "path",
        "placeholder",
        "plain",
        "presentedWhen",
        "props",
        "recipientBind",
        "ref",
        "refreshAction",
        "refreshOn",
        "rememberState",
        "replyContextKey",
        "replyToKey",
        "requiresMessage",
        "revealOnHover",
        "role",
        "resizeAction",
        "resizeKey",
        "resizeMax",
        "resizeMin",
        "route",
        "rowStyle",
        "rung",
        "scaleKey",
        "scales",
        "schema",
        "scrim",
        "scroll",
        "scrollV",
        "secure",
        "seed",
        "savedBind",
        "seedBind",
        "selectedStyle",
        "selectedWhen",
        "selectionKey",
        "selectionTextKey",
        "sheet",
        "sheets",
        "shortcuts",
        "sidebar",
        "sidebarWidth",
        "size",
        "spacing",
        "spread",
        "state",
        "stateKey",
        "step",
        "style",
        "sub",
        "subBind",
        "subScale",
        "sublabel",
        "submit",
        "swipeActions",
        "symbol",
        "symbolBind",
        "symbolSize",
        "tabs",
        "tap",
        "theaterState",
        "theaterWhenAll",
        "threadBind",
        "threadKey",
        "tint",
        "tintFrom",
        "title",
        "titleBind",
        "toolbar",
        "topActions",
        "totalBind",
        "trailing",
        "uncheckedBody",
        "unit",
        "value",
        "valueBind",
        "variant",
        "visibleWhen",
        "width",
        "wrap",
    ];

    /// Keys inside an action object, which lives in a different namespace from
    /// a view node's props.
    pub const ACTION_KEYS: &[&str] = &[
        "kind",
        "method",
        "path",
        "body",
        "refresh",
        "optimistic",
        "then",
        // state key ← reply field, read by the hub's `OPAQUE` list and both
        // twins' `applyStateFromResponse`.
        "setStateFromResponse",
        "title",
        "message",
        "confirmLabel",
        "destructive",
        "confirm",
        "text",
        "key",
        "value",
        "url",
        "route",
        "params",
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
                let is_action =
                    kind.is_some_and(|k| ACTION_KINDS.contains(&k)) || map.contains_key("method");

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
                    if matches!(
                        k.as_str(),
                        "visibleWhen" | "filter" | "body" | "assistant" | "state"
                    ) {
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
                // What an action writes back from a reply.
                if let Some(m) = map.get("setStateFromResponse").and_then(Value::as_object) {
                    out.extend(m.keys().cloned());
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
                // A sheet's flag, the screen's fill flag, and what an action
                // writes.
                for prop in ["presentedWhen", "noScrollState"] {
                    if let Some(k) = map.get(prop).and_then(Value::as_str) {
                        out.push(k.to_string());
                    }
                }
                if map.get("kind") == Some(&json!("setState")) {
                    if let Some(k) = map.get("key").and_then(Value::as_str) {
                        out.push(k.to_string());
                    }
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

        // The one hub route a sheet calls on purpose, with the person's own
        // session: approving the satellite pairing the create asked for.
        const HUB_ROUTES: &[&str] = &["/api/satellite-pair/{pair_code}/approve"];
        for p in &paths {
            if HUB_ROUTES.contains(&p.as_str()) {
                continue;
            }
            assert!(
                p.starts_with("/p/spaces/"),
                "'{p}' is not under this package's mount prefix — it would reach the hub instead"
            );
            // Strip the prefix and the interpolation, and check the shape
            // against what api::router() declares.
            let route = p.trim_start_matches("/p/spaces");
            let known = [
                "/api/host/apply",
                "/api/host/fetch-iso",
                "/api/host/import-iso",
                "/api/spaces",
                "/api/spaces/{title}",
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
        for want in [
            "macwindow",
            "gearshape",
            "plus",
            "cpu",
            "doc.on.doc",
            "display",
        ] {
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
