//! The row the store publishes has to be the package this repo builds.
//!
//! Publishing is an edit on `admin.leoconnect.io`: the catalog at
//! `registry.leoconnect.io/registry.json` is that console's active rows served
//! directly, and `install_from_registry` hands the row's `descriptor` to
//! `perform_dynamic_install` **verbatim** — the hub clones this repo for its
//! *code* and never reads its `descriptor.json`.
//!
//! So a store entry that has drifted is not a stale listing. It is a different
//! package wearing this one's name: an entry missing `ui` installs a nav page
//! with no tree, one missing `fields` installs with no Host setting, one
//! missing `entitlements` installs unable to read its own settings — each of
//! them silent, on somebody else's hub, with this repo looking correct.
//!
//! Regenerate with `scripts/sync-registry-entry.sh`.

use serde_json::Value;

fn read(name: &str) -> Value {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(name);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{name}: {e}"));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{name} is not JSON: {e}"))
}

#[test]
fn the_store_entry_carries_this_descriptor() {
    let entry = read("store/registry-entry.json");
    let descriptor = read("descriptor.json");

    assert_eq!(
        entry.get("descriptor"),
        Some(&descriptor),
        "store/registry-entry.json has drifted from descriptor.json — \
         run scripts/sync-registry-entry.sh"
    );
    assert_eq!(entry["name"], descriptor["name"]);
    assert_eq!(entry["label"], descriptor["label"]);
}

/// The store install creates the MCP server from the entry's `mcp` block, and
/// the only path that exists on the installing hub is the one the hub itself
/// builds the app to.
#[test]
fn the_entry_launches_the_binary_the_hub_builds() {
    let entry = read("store/registry-entry.json");
    let app = &entry["descriptor"]["app"];
    let expected = format!(
        "/var/lib/leo/apps/{}/src/{}",
        entry["name"].as_str().unwrap(),
        app["bin"].as_str().unwrap()
    );
    assert_eq!(entry["mcp"]["command"], Value::String(expected));
    assert_eq!(entry["mcp"]["args"], serde_json::json!(["mcp"]));
    assert_eq!(entry["mcp"]["transport"], "stdio");
}

/// The catalog requires a pinned source: `app.rev` is a full 40-hex commit, and
/// the hub re-reads `HEAD` after fetching to confirm it got that exact one.
#[test]
fn the_published_source_is_pinned_to_a_full_commit() {
    let entry = read("store/registry-entry.json");
    let rev = entry["descriptor"]["app"]["rev"]
        .as_str()
        .unwrap_or_default();
    assert_eq!(
        rev.len(),
        40,
        "app.rev must be a full commit sha, got '{rev}'"
    );
    assert!(
        rev.chars().all(|c| c.is_ascii_hexdigit()),
        "app.rev must be hex, got '{rev}'"
    );
}

/// The version in the listing is the version of the crate it installs.
#[test]
fn the_listed_version_is_the_crate_version() {
    let entry = read("store/registry-entry.json");
    let cargo = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"),
    )
    .unwrap();
    let version = cargo
        .lines()
        .find_map(|l| l.strip_prefix("version = \""))
        .and_then(|v| v.strip_suffix('"'))
        .expect("Cargo.toml has a version");
    assert_eq!(entry["version"], version);
}

/// The helper digest in the entry is the digest of the helper that ships.
///
/// The hub checks `scripts/leo-spaces-admin` against
/// `privileged_helper.sha256` before it will promote the script to root, and
/// refuses the whole install when they differ. A pin commit once rewrote the
/// first forty hex digits of every hash-shaped string in `descriptor.json`,
/// which turned this field into a commit hash wearing the digest's tail —
/// and `the_store_entry_carries_this_descriptor` shipped it faithfully,
/// because the entry did equal the descriptor. Computed here with the same
/// tool the error message tells an operator to run.
#[test]
fn the_helper_digest_is_the_helpers() {
    let entry = read("store/registry-entry.json");
    let declared = entry["descriptor"]["privileged_helper"]["sha256"]
        .as_str()
        .expect("the entry declares its helper's sha256");
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/leo-spaces-admin");
    let out = std::process::Command::new("sha256sum")
        .arg(&script)
        .output()
        .expect("sha256sum runs");
    let actual = String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_string();
    assert_eq!(
        declared, actual,
        "privileged_helper.sha256 is not the digest of scripts/leo-spaces-admin — \
         every hub refuses the install; fix it with: sha256sum scripts/leo-spaces-admin"
    );
    let rev = entry["descriptor"]["app"]["rev"].as_str().unwrap_or_default();
    assert!(
        !declared.starts_with(rev),
        "the helper digest begins with the pinned commit — a pin rewrote it"
    );
}
