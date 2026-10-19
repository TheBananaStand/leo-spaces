// The console page's script: bundled with noVNC into one ES module
// (`npx esbuild assets/console.js --bundle --format=esm --minify
// --outfile=assets/novnc.bundle.js`) and inlined into the page by
// `src/console.rs`. Everything the page needs to know arrives on <body>'s
// data attributes, so this file carries no template.
import RFB from "./novnc/core/rfb.js";

const { name, note } = document.body.dataset;
const status = document.getElementById("status");
const msg = document.getElementById("msg");
const bar = document.getElementById("bar");
const field = document.getElementById("send");
const say = (text, bad) => {
  msg.textContent = text;
  status.classList.toggle("bad", !!bad);
  status.classList.remove("hidden");
};

// X11 keysyms for the characters a keyboard sends that are not themselves.
const NAMED = { "\n": 0xff0d, "\r": 0xff0d, "\t": 0xff09, "\b": 0xff08 };

/// The keysym for one character. Printable ASCII IS its code point; anything
/// above it uses the Unicode plane X11 reserves for exactly this.
function keysym(ch) {
  if (NAMED[ch] !== undefined) return NAMED[ch];
  const cp = ch.codePointAt(0);
  return cp < 0x100 ? cp : 0x01000000 + cp;
}

/// Type text into the guest as key events, one press and release each.
///
/// Key events rather than the clipboard, because the clipboard cannot work
/// where this is most needed: RFB clipboard text is only picked up by an
/// agent running inside a signed-in session, so at a Windows **login**
/// screen — the one place you must type a 20-character generated password —
/// a paste arrives nowhere. Keystrokes are the login screen's own language.
///
/// The small gap is not superstition: a guest that is still waking its
/// input stack drops a burst sent in one tick, and the symptom is a password
/// missing its first few characters, which reads as the wrong password.
async function type(rfb, text) {
  await wake(rfb);
  for (const ch of text) {
    const k = keysym(ch);
    rfb.sendKey(k, null, true);
    rfb.sendKey(k, null, false);
    await new Promise((r) => setTimeout(r, 12));
  }
}

/// Wake a lock screen without typing into it.
///
/// A Windows lock screen takes the first key it sees to *dismiss itself* and
/// swallows it — and the password box is still sliding in while the rest of
/// a 20-character burst arrives, so it lands short by its first few characters
/// and reads as "the wrong password". Seen on a real guest: the lock screen
/// went away and the field stayed empty.
///
/// A bare Shift is the one key that is not a character: it dismisses the lock
/// screen exactly as any key does, and at a password prompt — or anywhere else
/// — it types nothing. The pause after it is the slide-in; measured against
/// Windows 11, 600 ms is comfortably past it and well under what anyone notices
/// on a paste.
async function wake(rfb) {
  const SHIFT_L = 0xffe1;
  rfb.sendKey(SHIFT_L, null, true);
  rfb.sendKey(SHIFT_L, null, false);
  await new Promise((r) => setTimeout(r, 600));
}

if (note) {
  say(note, false);
} else {
  say("Connecting…", false);
  const proto = location.protocol === "https:" ? "wss:" : "ws:";
  // The session rides in the URL: a socket opened from a page carries no
  // header the panel stamped on the main frame, and a cookie never reaches
  // the hub through the edge. The proxy reads `?token=` on an upgrade.
  const token = new URLSearchParams(location.search).get("token") || "";
  const url =
    `${proto}//${location.host}/p/spaces/api/spaces/${encodeURIComponent(name)}/console/ws` +
    (token ? `?token=${encodeURIComponent(token)}` : "");
  // No subprotocol: the hub's bridge forwards none, and a browser refuses a
  // handshake that asked for one and was not answered.
  const rfb = new RFB(document.getElementById("screen"), url, { wsProtocols: [] });
  rfb.scaleViewport = true;
  rfb.resizeSession = false;
  // A socket the edge refuses is often held, not closed — so "Connecting…"
  // would sit there forever. After a while, say what it almost certainly is.
  let connected = false;
  const patience = setTimeout(() => {
    if (!connected)
      say("Still connecting. If this hub is reached through its tunnel, the edge will not pass the socket — connect to Leo's VPN and open the console again.", true);
  }, 12000);
  rfb.addEventListener("connect", () => {
    connected = true;
    clearTimeout(patience);
    status.classList.add("hidden");
    bar.classList.remove("hidden");
  });
  rfb.addEventListener("disconnect", (e) => {
    bar.classList.add("hidden");
    say(e.detail.clean
      ? "Disconnected."
      : "Could not reach the console — the space may be off, or this hub is reached through its tunnel, which refuses the socket at the edge.", !e.detail.clean);
  });
  rfb.addEventListener("securityfailure", (e) => say(`Refused: ${e.detail.reason}`, true));

  const send = async () => {
    const text = field.value;
    if (!text) return;
    field.value = "";
    field.disabled = true;
    // Also hand it to the guest's clipboard, so that once somebody IS signed
    // in an ordinary Ctrl+V works for the next one without coming back here.
    try { rfb.clipboardPasteFrom(text); } catch {}
    await type(rfb, text);
    field.disabled = false;
    rfb.focus();
  };
  field.addEventListener("keydown", (e) => {
    // Typed INTO the field, so the guest must not also see these. Enter sends.
    e.stopPropagation();
    if (e.key === "Enter") { e.preventDefault(); send(); }
  });
  document.getElementById("go").addEventListener("click", send);
  document.getElementById("cad").addEventListener("click", () => {
    rfb.sendCtrlAltDel();
    rfb.focus();
  });
}
