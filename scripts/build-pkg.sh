#!/usr/bin/env bash
# Package the HOSTED Spaces as a pacman package, and write its store entry.
#
#   bash scripts/build-pkg.sh [outdir]
#
# Input: host/target/release/leo-spaces, already built. Build it first —
#
#   on a Leo hub (bare cargo is refused there; this routes it and takes the
#   build lane):
#     cd /opt/leo/leo && leo check build --release \
#         --manifest-path <this repo>/host/Cargo.toml
#   anywhere else with the Leo checkout at /opt/leo/leo:
#     cargo build --release --manifest-path host/Cargo.toml
#
# This script compiles nothing, on purpose: it is the packaging half, and the
# box it runs on decides how a compile may happen.
#
# Output, in [outdir] (default target/pkg):
#   leo-spaces-<ver>-1-x86_64.pkg.tar.zst   (+ .sig when the box has a key)
#   ../store/hosted-entry.json              the catalog row, § 3 `hosted` block
#
# The package itself is made by Leo's own `scripts/build-package-pkg.sh` —
# sysusers account, binary, artifact, bus policy, `depends=leo-runtime` — given
# this repo's binary through LEO_PACKAGE_BIN (the out-of-tree path, 50a6091f3).
# Nothing about the package's shape is restated here.
set -euo pipefail

say() { printf '\033[38;2;224;179;65m▸ %s\033[0m\n' "$*"; }
die() { printf '\033[31m✗ %s\033[0m\n' "$*" >&2; exit 1; }

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
LEO="${LEO_CHECKOUT:-/opt/leo/leo}"
OUT="${1:-$ROOT/target/pkg}"
BIN="$ROOT/host/target/release/leo-spaces"

[ -x "$BIN" ] || die "$BIN is not built — see the header for the build command"
[ -f "$LEO/scripts/build-package-pkg.sh" ] || die "no Leo checkout at $LEO (set LEO_CHECKOUT)"

# The binary must be the HOSTED one. The root crate's app-shape binary has the
# same name, and packaging it would ship a process that ignores both sockets.
# The hosted binary answers --artifact; the app-shape one would start a server.
WORK="$(mktemp -d)"
trap 'rm -rf -- "$WORK"' EXIT
LEO_DATA_DIR="$WORK" LEO_PACKAGE_NAME=spaces timeout 30 "$BIN" --artifact >"$WORK/artifact.json" 2>"$WORK/artifact.err" \
  || { sed 's/^/    /' "$WORK/artifact.err" >&2; die "$BIN --artifact failed — is this the hosted build?"; }
python3 -c 'import json,sys; a=json.load(open(sys.argv[1])); assert a["descriptor"]["name"]=="spaces", a["descriptor"].get("name")' "$WORK/artifact.json" \
  || die "$BIN --artifact did not describe the spaces package"

mkdir -p "$OUT"
say "packaging with $LEO/scripts/build-package-pkg.sh"
LEO_PACKAGE_BIN="$BIN" bash "$LEO/scripts/build-package-pkg.sh" spaces "$OUT" | tee "$WORK/build.log"

PKG="$(ls -t "$OUT"/leo-spaces-*.pkg.tar.zst 2>/dev/null | grep -v '\.sig$' | head -1 || true)"
[ -n "$PKG" ] || die "build-package-pkg.sh produced no leo-spaces package in $OUT"

# The § 3 block, from the file itself: pkgname/pkgver/arch out of its own
# .PKGINFO, the digest and size of the bytes. The URL is where
# `leo packages publish` puts every package artifact (RELEASE_REPO /
# RELEASE_TAG in crates/leo-cli/src/package_publish.rs) — this script does
# not upload; publishing is the owner's call.
python3 - "$PKG" "$WORK/artifact.json" "$ROOT/store/hosted-entry.json" <<'PY'
import collections, hashlib, json, os, subprocess, sys
pkg, artifact_path, out = sys.argv[1:4]
info = {}
for line in subprocess.run(["bsdtar", "-xOf", pkg, ".PKGINFO"], capture_output=True, text=True, check=True).stdout.splitlines():
    if " = " in line:
        k, v = line.split(" = ", 1)
        info.setdefault(k, v)
filename = f"{info['pkgname']}-{info['pkgver']}-{info['arch']}.pkg.tar.zst"
assert os.path.basename(pkg) == filename, (pkg, filename)
sha = hashlib.sha256(open(pkg, "rb").read()).hexdigest()
art = json.load(open(artifact_path), object_pairs_hook=collections.OrderedDict)
d = art["descriptor"]
keys = ["feed", "push", "settings_read", "network", "sends", "storage_mb",
        "graph_scopes", "oauth_tokens", "satellite_capabilities", "inference"]
ent = collections.OrderedDict()
for k in keys:
    v = (d.get("entitlements") or {}).get(k)
    if v in (None, False, [], {}, 0):
        continue
    ent[k] = v
entry = collections.OrderedDict([
    ("name", "spaces"),
    ("label", d.get("label") or "Spaces"),
    ("description", d.get("description", "")),
    ("icon", d.get("icon", "")),
    ("version", info["pkgver"].rsplit("-", 1)[0]),
    ("author", "TheBananaStand"),
    ("homepage", "https://github.com/TheBananaStand/leo-spaces"),
    ("category", "package"),
    ("descriptor", collections.OrderedDict([("entitlements", ent)]) if ent else {}),
    ("hosted", collections.OrderedDict([
        ("pkgname", info["pkgname"]),
        ("version", info["pkgver"]),
        ("arch", info["arch"]),
        ("url", f"https://github.com/TheBananaStand/leo-release/releases/download/packages/{filename}"),
        ("sha256", sha),
        ("size_bytes", os.path.getsize(pkg)),
    ])),
])
with open(out, "w") as f:
    json.dump(entry, f, indent=2, ensure_ascii=False)
    f.write("\n")
print(f"store/hosted-entry.json: {info['pkgname']} {info['pkgver']} sha256 {sha[:12]}… {os.path.getsize(pkg)} bytes")
PY
