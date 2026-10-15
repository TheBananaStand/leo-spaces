#!/usr/bin/env bash
#
# The one privileged step the Spaces package cannot take for itself.
#
#   sudo bash scripts/install-admin-wrapper.sh
#
# Installs scripts/leo-spaces-admin root-owned at /usr/local/bin, and writes
# the single sudoers rule that lets the hub's service account run it — and
# nothing else — with no password and no TTY.
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

die() { printf 'install-admin-wrapper: %s\n' "$*" >&2; exit 1; }
say() { printf '%s\n' "$*"; }

[ "$(id -u)" = "0" ] || die "run me with sudo"

# Which account the hub's Spaces process runs as. Defaults to the Leo service
# account; override for a box where the hub runs as something else.
SERVICE_USER="${LEO_SPACES_USER:-leo}"

if [ "${1:-}" = "--remove" ]; then
  rm -f "$SUDOERS" "$WRAPPER_DST"
  say "removed $SUDOERS and $WRAPPER_DST"
  say "the Spaces package drops back to read-only: it will still report and plan."
  exit 0
fi

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

say ""
say "Done. Open Spaces in Leo — Host setup now has working buttons."
