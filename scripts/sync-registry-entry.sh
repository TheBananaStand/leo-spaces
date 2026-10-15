#!/usr/bin/env bash
#
# Write store/registry-entry.json from descriptor.json.
#
#   bash scripts/sync-registry-entry.sh
#
# WHAT THIS FILE IS FOR. It is the row to paste into the store console at
# admin.leoconnect.io, which is the whole publishing mechanism — the catalog at
# registry.leoconnect.io/registry.json is that D1's active rows, served
# directly. There is no repo in the loop and no PR to open.
#
# WHY IT CARRIES THE WHOLE DESCRIPTOR. `install_from_registry` hands
# `entry.descriptor` to `perform_dynamic_install` verbatim; the hub clones this
# repo for the *code* and never reads its `descriptor.json`. So whatever this
# entry omits, an installed-from-the-store copy does not have: no `ui` means no
# page, no `fields` means no Host setting, no `entitlements` means it may read
# nothing. Generated rather than hand-kept for exactly that reason.
set -euo pipefail

cd "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/.."

python3 - "$PWD" <<'PY'
import collections, json, re, sys, pathlib

root = pathlib.Path(sys.argv[1])
descriptor = json.loads(
    (root / "descriptor.json").read_text(), object_pairs_hook=collections.OrderedDict
)

cargo = (root / "Cargo.toml").read_text()
version = re.search(r'^version\s*=\s*"([^"]+)"', cargo, re.M).group(1)

repo = descriptor["app"]["repo"]
# The path the hub builds an app package to: {data_dir}/apps/<name>/src/<bin>.
# The MCP server is launched from there, unjailed, which is why the tools work
# whatever the HTTP half is doing.
command = f"/var/lib/leo/apps/{descriptor['name']}/src/{descriptor['app']['bin']}"

entry = collections.OrderedDict(
    [
        ("name", descriptor["name"]),
        ("label", descriptor["label"]),
        ("description", descriptor["description"]),
        ("icon", descriptor["icon"]),
        ("version", version),
        ("author", repo.rstrip("/").split("/")[-2]),
        ("homepage", repo),
        ("category", descriptor.get("category", "package")),
        (
            "mcp",
            collections.OrderedDict(
                [("transport", "stdio"), ("command", command), ("args", ["mcp"])]
            ),
        ),
        ("descriptor", descriptor),
    ]
)

out = root / "store" / "registry-entry.json"
out.write_text(json.dumps(entry, indent=2, ensure_ascii=False) + "\n")
print(f"store/registry-entry.json: {descriptor['name']} {version}, rev {descriptor['app']['rev'][:8]}")
PY
