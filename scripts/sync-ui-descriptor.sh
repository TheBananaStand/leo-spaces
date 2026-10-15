#!/usr/bin/env bash
#
# Write the page tree from ui.rs into descriptor.json's `ui` field.
#
#   bash scripts/sync-ui-descriptor.sh
#
# WHY THE TREE IS EMBEDDED AT ALL. The hub renders a native page from the
# descriptor it has stored — `ui_descriptor()` reads that field and nothing
# else. It never asks an app package for its tree; only `GET /leo/ui/data` is
# proxied to the process. So `/leo/ui/descriptor` is the live copy and this is
# the one the clients draw, and they have to be the same tree.
#
# Run this after any change to ui.rs. `cargo test` fails when they differ
# (the_embedded_descriptor_matches_this_code), because every mistake in a
# descriptor is silent: a page drawn from an old tree binds today's data into
# yesterday's rows and logs nothing.
set -euo pipefail

cd "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/.."

cargo build --release
./target/release/leo-spaces ui-descriptor > /tmp/leo-spaces-ui.json

python3 - "$PWD/descriptor.json" /tmp/leo-spaces-ui.json <<'PY'
import collections, json, sys

descriptor_path, ui_path = sys.argv[1], sys.argv[2]
with open(descriptor_path) as f:
    d = json.load(f, object_pairs_hook=collections.OrderedDict)
with open(ui_path) as f:
    ui = json.load(f, object_pairs_hook=collections.OrderedDict)

# Every page the descriptor advertises needs a tree, or it renders nothing.
declared = [p["path"] for p in d.get("pages", [])]
missing = [p for p in declared if p not in ui]
if missing:
    sys.exit(f"ui.rs has no tree for declared page(s): {', '.join(missing)}")

d["ui"] = ui
with open(descriptor_path, "w") as f:
    json.dump(d, f, indent=2, ensure_ascii=False)
    f.write("\n")
print(f"descriptor.json: ui = {', '.join(ui)}")
PY

rm -f /tmp/leo-spaces-ui.json
