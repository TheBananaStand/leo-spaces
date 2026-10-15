#!/usr/bin/env bash
#
# The one privileged step the Spaces package cannot take for itself.
#
#   sudo bash scripts/install-admin-wrapper.sh [--token FILE]
#
# Installs scripts/leo-spaces-admin root-owned at /usr/local/bin, and writes
# the single sudoers rule that lets the hub's service account run it — and
# nothing else — with no password and no TTY.
#
# With --token FILE it also installs a package token at /etc/leo-spaces/token,
# which is how this package reads its own settings. It needs one because it
# declares a *fixed* port: the hub hands entitled settings to a dynamic-port
# app package and nothing at all to a fixed-port one, and a fixed port is what
# keeps it out of the bubblewrap jail that cannot reach libvirtd. Mint the
# token first, as an admin:
#
#   leo packages token mint spaces --scope settings:read --label spaces > /tmp/t
#
# Only needed to drive a *remote* host — the four spaces_* settings describe
# nothing else. Virtualising on this box needs no token.
#
# Undo with:
#   sudo bash scripts/install-admin-wrapper.sh --remove
#
# WHAT THIS GRANT IS, EXACTLY. One rule naming one root-owned program with a
# closed verb set (see the header of leo-spaces-admin). It is narrow because
# the program takes verbs rather than commands: the only caller-supplied value
# any verb accepts is a PCI `vendor:device` pair, matched against a regex
# before it is written anywhere. The program is root-owned and mode 0755, so
# the account it is granted to cannot edit what it is permitted to run — which
# is the property the whole thing rests on, and the first thing this script
# verifies on the way out.

set -euo pipefail

WRAPPER_SRC="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/leo-spaces-admin"
WRAPPER_DST=/usr/local/bin/leo-spaces-admin
SUDOERS=/etc/sudoers.d/leo-spaces
CONF_DIR=/etc/leo-spaces
TOKEN_DST="$CONF_DIR/token"

die() { printf 'install-admin-wrapper: %s\n' "$*" >&2; exit 1; }
say() { printf '%s\n' "$*"; }

[ "$(id -u)" = "0" ] || die "run me with sudo"

# Which account the hub's Spaces process runs as. Defaults to the Leo service
# account; override for a box where the hub runs as something else.
SERVICE_USER="${LEO_SPACES_USER:-leo}"

TOKEN_SRC=""
case "${1:-}" in
  --remove)
    rm -f "$SUDOERS" "$WRAPPER_DST" "$TOKEN_DST"
    rmdir "$CONF_DIR" 2>/dev/null || true
    say "removed $SUDOERS, $WRAPPER_DST and $TOKEN_DST"
    say "the Spaces package drops back to read-only: it will still report and plan."
    say "revoke the token too — this script cannot: leo packages token list spaces"
    exit 0
    ;;
  --token)
    TOKEN_SRC="${2:-}"
    [ -n "$TOKEN_SRC" ] || die "--token needs a file (the output of: leo packages token mint spaces --scope settings:read)"
    [ -s "$TOKEN_SRC" ] || die "$TOKEN_SRC is empty or missing"
    ;;
  "") ;;
  *) die "unknown argument '$1' (expected --token FILE or --remove)" ;;
esac

[ -f "$WRAPPER_SRC" ] || die "cannot find $WRAPPER_SRC — run this from the package checkout"
id -u "$SERVICE_USER" >/dev/null 2>&1 \
  || die "no such account: $SERVICE_USER (set LEO_SPACES_USER if the hub runs as someone else)"

# install(1) rather than cp: it sets owner and mode atomically, so there is no
# instant where the file exists and is writable by whoever built it.
install -o root -g root -m 0755 "$WRAPPER_SRC" "$WRAPPER_DST"
say "installed $WRAPPER_DST (root:root 0755)"

# Written through a temp file and validated before it is moved into place. A
# malformed file in /etc/sudoers.d breaks sudo for *everyone* on the box,
# including the root shell that would be needed to repair it.
TMP=$(mktemp)
cat > "$TMP" <<EOF
# Leo Spaces — hypervisor provisioning.
#
# One root-owned program with a closed verb set. Written by
# scripts/install-admin-wrapper.sh; remove with that script's --remove.
$SERVICE_USER ALL=(root) NOPASSWD: $WRAPPER_DST
EOF
chmod 0440 "$TMP"

if command -v visudo >/dev/null 2>&1; then
  visudo -cqf "$TMP" || { rm -f "$TMP"; die "refusing to install a sudoers file that does not parse"; }
fi
install -o root -g root -m 0440 "$TMP" "$SUDOERS"
rm -f "$TMP"
say "wrote $SUDOERS"

# Prove the grant works as the account that will use it, rather than asserting
# it. A rule that parses and still does not apply — wrong user, an earlier
# blanket deny, an #includedir that does not cover sudoers.d — is the failure
# this catches, and it is invisible until the first button press.
if sudo -n -u "$SERVICE_USER" sudo -n "$WRAPPER_DST" version >/dev/null 2>&1; then
  say "verified: $SERVICE_USER can run $(basename "$WRAPPER_DST") with no password"
else
  die "the rule is installed but $SERVICE_USER still cannot run it — check for a later rule in /etc/sudoers that overrides sudoers.d"
fi

# The grant is only narrow while the granted account cannot rewrite the
# program. Checked here because a chmod somewhere else is what would quietly
# turn this into a root shell.
owner=$(stat -c '%U:%G %a' "$WRAPPER_DST")
[ "$owner" = "root:root 755" ] || die "unexpected ownership on $WRAPPER_DST: $owner"
say "verified: $WRAPPER_DST is root-owned and not writable by $SERVICE_USER"

# The token, if one was handed over. Root-owned in a directory the service
# account may read and may not write, and only its *contents* travel: the file
# the admin minted into stays theirs to shred.
if [ -n "$TOKEN_SRC" ]; then
  install -o root -g root -m 0755 -d "$CONF_DIR"
  umask 077
  TTMP=$(mktemp)
  tr -d '\r\n' < "$TOKEN_SRC" > "$TTMP"
  [ -s "$TTMP" ] || { rm -f "$TTMP"; die "$TOKEN_SRC held no token"; }
  install -o root -g "$SERVICE_USER" -m 0640 "$TTMP" "$TOKEN_DST"
  rm -f "$TTMP"
  say "installed $TOKEN_DST (root:$SERVICE_USER 0640)"

  # Readable by the account that will read it, and by nobody else. Both halves
  # checked, because a 0640 file in a directory that account cannot enter is
  # the failure that reads as "the setting has no effect" with nothing logged.
  if sudo -n -u "$SERVICE_USER" head -c1 "$TOKEN_DST" >/dev/null 2>&1; then
    say "verified: $SERVICE_USER can read the token"
  else
    die "$SERVICE_USER cannot read $TOKEN_DST — check the mode on $CONF_DIR"
  fi
  say "the settings it unlocks are the four the descriptor declares; nothing else"
fi

say ""
say "Done. Open Spaces in Leo — Host setup now has working buttons."
if [ -z "$TOKEN_SRC" ]; then
  say ""
  say "No token installed, so the Host setting cannot be read: this will"
  say "virtualise on $(hostname) itself. To drive a remote host instead, mint"
  say "one and re-run with --token (see the header of this script)."
fi
