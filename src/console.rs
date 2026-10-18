//! The guest's screen, in a window: `GET /console/{name}` is a page that
//! draws it, `GET /api/spaces/{name}/console/ws` is the bytes behind it.
//!
//! A guest's graphics are a VNC server on the host's loopback — loopback only,
//! because a graphics port on a LAN address is an unauthenticated console for
//! anyone who can route to it. This is the one road to it: the hub
//! authenticates the person (an admin, since a console is the whole machine),
//! proxies the page and its WebSocket under `/p/spaces/`, and this process
//! carries the socket's bytes to the host over the ssh it already holds
//! ([`crate::host::Host::tcp_pipe`]). Nothing new listens anywhere.
//!
//! The page is noVNC (`assets/novnc`, vendored; `assets/novnc.bundle.js` is
//! `assets/console.js` plus the noVNC it imports, bundled by esbuild into one
//! ES module), which speaks RFB
//! over a WebSocket. **The page is one document with no subresources, and
//! the socket carries the session in its URL.** Both for the same reason: the
//! Mac opens it in a web panel that can stamp the main frame — with the
//! session token, and with the Cloudflare Access service token when the
//! world is reached over its tunnel — and can stamp nothing else. A
//! `<script src>` or a cookie-authenticated socket is refused at the edge
//! before the hub ever sees it. So the bundle is inlined, and the socket
//! URL repeats the `?token=` the main frame arrived with (the proxy strips
//! it before forwarding, but `location.search` in the page still has it,
//! and the proxy accepts it on an upgrade).

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::auth::AdminCaller;
use crate::error::Error;
use crate::{spaces, AppState};

/// Where the noVNC files are: beside the binary's package (`…/src/assets/novnc`
/// when the hub built it), or `assets/novnc` under the working directory for a
/// by-hand run. `LEO_SPACES_ASSETS` overrides.
pub fn novnc_dir() -> std::path::PathBuf {
    if let Some(p) = std::env::var_os("LEO_SPACES_ASSETS") {
        return std::path::PathBuf::from(p).join("novnc");
    }
    let from_exe = std::env::current_exe().ok().and_then(|exe| {
        exe.ancestors()
            .find(|a| a.join("assets").join("novnc").join("core").is_dir())
            .map(|a| a.join("assets").join("novnc"))
    });
    from_exe.unwrap_or_else(|| std::path::PathBuf::from("assets/novnc"))
}

/// The page: one document, the bundle inlined, nothing else to fetch.
pub async fn page(
    _c: AdminCaller,
    State(s): State<AppState>,
    Path(name): Path<String>,
) -> Result<Response, Error> {
    let space = spaces::get(&s.cfg.host, &name).await?;
    Ok(Html(render(
        &space.name,
        space.state.is_running(),
        space.console_port.is_some(),
    ))
    .into_response())
}

fn render(name: &str, running: bool, has_console: bool) -> String {
    let safe = esc(name);
    let note = if !running {
        "This space is off. Start it from Spaces, then reopen the console."
    } else if !has_console {
        "This space has no emulated display, so there is nothing to show here."
    } else {
        ""
    };
    PAGE.replace("{{name}}", &safe)
        .replace("{{note}}", note)
        .replace("{{bundle}}", NOVNC_BUNDLE)
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// The page's whole script: `assets/console.js` (the logic) bundled with the
/// noVNC it imports into one ES module — `cd assets && npx esbuild console.js
/// --bundle --format=esm --minify --outfile=novnc.bundle.js`. Inlined, so the
/// page is one document. One module rather than the bundle plus a second
/// script: noVNC needs top-level await (so it cannot be a classic script),
/// and an inlined module's `export` leaves nothing in scope for a script
/// after it — the first cut's `RFB` was undefined, and the page died one line
/// after saying "Connecting…". The name and the note reach the script on
/// `<body>`'s data attributes.
const NOVNC_BUNDLE: &str = include_str!("../assets/novnc.bundle.js");

const PAGE: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{{name}} — console</title>
<style>
  :root { color-scheme: dark; }
  html, body { height: 100%; margin: 0; background: #0e0e10; color: #ececee;
    font: 400 13px/1.4 -apple-system, BlinkMacSystemFont, "SF Pro Text", system-ui, sans-serif; }
  #screen { position: absolute; inset: 0; }
  #status { position: absolute; left: 0; right: 0; top: 0; padding: 10px 14px;
    background: rgba(27,27,29,.85); color: #98989d; font-size: 12px;
    display: flex; gap: 12px; align-items: center; pointer-events: none; }
  #status.hidden { display: none; }
  #status b { color: #ececee; font-weight: 600; }
  #status.bad { color: #ff453a; }
  /* The type bar. A VNC client cannot paste into a guest the way the rest of
     the desktop can — and at a login screen the clipboard is not even read —
     so this is where text gets in: it is typed as key events. */
  #bar { position: absolute; left: 0; right: 0; bottom: 0; padding: 8px 10px;
    background: rgba(27,27,29,.92); display: flex; gap: 8px; align-items: center; }
  #bar.hidden { display: none; }
  #bar input { flex: 1; min-width: 0; padding: 6px 9px; border-radius: 7px;
    border: 1px solid #3a3a3d; background: #141416; color: #ececee;
    font: inherit; }
  #bar input::placeholder { color: #727275; }
  #bar button { padding: 6px 11px; border-radius: 7px; border: 1px solid #3a3a3d;
    background: #242427; color: #ececee; font: inherit; cursor: pointer;
    white-space: nowrap; }
  #bar button:hover { background: #2e2e32; }
</style>
</head>
<body data-name="{{name}}" data-note="{{note}}">
<div id="screen"></div>
<div id="status"><b>{{name}}</b><span id="msg">{{note}}</span></div>
<div id="bar" class="hidden">
  <input id="send" type="text" autocomplete="off" spellcheck="false"
         placeholder="Type text into this machine — a password, a command — then Enter">
  <button id="go" type="button">Send</button>
  <button id="cad" type="button" title="Ctrl+Alt+Delete">⌃⌥⌦</button>
</div>
<script type="module">
{{bundle}}
</script>
</body>
</html>
"#;

/// The bytes. Admin-only, like every control on a space: a console IS the
/// machine.
pub async fn socket(
    _c: AdminCaller,
    State(s): State<AppState>,
    Path(name): Path<String>,
    ws: WebSocketUpgrade,
) -> Result<Response, Error> {
    let space = spaces::get(&s.cfg.host, &name).await?;
    if !space.state.is_running() {
        return Err(Error::Space(spaces::SpaceError::Refused(format!(
            "'{name}' is not running"
        ))));
    }
    let Some(port) = space.console_port else {
        return Err(Error::Space(spaces::SpaceError::Refused(format!(
            "'{name}' has no console"
        ))));
    };
    // The pipe is opened before the upgrade, so a host that cannot be reached
    // is an HTTP error the page can show rather than a socket that closes.
    let pipe = s.cfg.host.tcp_pipe(port).await?;
    Ok(ws
        .max_message_size(16 << 20)
        .on_upgrade(move |socket| pump(socket, pipe)))
}

/// Pump bytes both ways until either side closes.
async fn pump(socket: WebSocket, pipe: crate::host::Pipe) {
    let (mut from_host, mut to_host, child) = pipe.split();
    let (mut ws_tx, mut ws_rx) = socket.split();

    let host_to_ws = async {
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            match from_host.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if ws_tx
                        .send(Message::Binary(bytes::Bytes::copy_from_slice(&buf[..n])))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
            }
        }
    };
    let ws_to_host = async {
        while let Some(Ok(msg)) = ws_rx.next().await {
            let data = match msg {
                Message::Binary(b) => b,
                Message::Text(t) => bytes::Bytes::from(t.to_string()),
                Message::Close(_) => break,
                _ => continue,
            };
            if to_host.write_all(&data).await.is_err() {
                break;
            }
        }
    };
    tokio::select! {
        _ = host_to_ws => {}
        _ = ws_to_host => {}
    }
    // `kill_on_drop` ends the ssh when the child drops; said explicitly so
    // the intent is in the code and not only in the spawn flags.
    if let Some(mut child) = child {
        let _ = child.start_kill();
    }
}

/// `GET /console` with no name: nothing to show, said rather than 404'd.
pub async fn index(_c: AdminCaller) -> impl IntoResponse {
    (StatusCode::NOT_FOUND, "name a space: /console/<name>")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The page is one document with the socket at an absolute path under
    /// the hub's mount, and the bundle inside it.
    #[test]
    fn the_page_is_one_document_with_the_bundle_inside() {
        let html = render("cad", true, true);
        assert!(
            html.contains("/p/spaces/api/spaces/"),
            "the socket path is under the mount"
        );
        assert!(html.contains("/console/ws"));
        let squeezed: String = html.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(
            squeezed.contains("wsProtocols:[]"),
            "the hub's bridge forwards no subprotocol"
        );
        // One document: nothing to fetch, because nothing fetched from a
        // panel page can carry what the main frame was stamped with.
        assert!(
            !html.contains("<script src"),
            "a subresource would be refused at the edge"
        );
        assert!(
            !html.contains("<link "),
            "a subresource would be refused at the edge"
        );
        assert!(
            !html.contains("import RFB from"),
            "the bundle is inlined, not imported"
        );
        assert!(
            !html.contains("export{"),
            "an inlined module's export leaves nothing in scope"
        );
        assert!(
            html.contains(r#"data-name="cad""#),
            "the script reads the name from <body>"
        );
        assert!(html.len() > 150_000, "the bundle is in the page");
        assert!(
            html.contains(r#"get("token")"#),
            "the socket carries the session in its URL"
        );
        assert!(
            html.contains("connect to Leo's VPN"),
            "a held socket is explained, not spun on"
        );
    }

    /// A space that is off, or has no display, says so on the page instead of
    /// opening a socket that will be refused.
    #[test]
    fn an_off_space_says_so_instead_of_connecting() {
        let off = render("cad", false, true);
        assert!(off.contains("This space is off"));
        let blind = render("cad", true, false);
        assert!(blind.contains("no emulated display"));
        assert!(!render("cad", true, true).contains("This space is off"));
    }

    /// A name reaches the document as text, never as markup.
    #[test]
    fn the_name_is_escaped() {
        let html = render("<img src=x onerror=alert(1)>", true, true);
        assert!(!html.contains("<img"));
        assert!(html.contains("&lt;img"));
    }
}
