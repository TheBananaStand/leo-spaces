//! Spaces as a `LeoPackage` — what `leo-package-host` runs as
//! `leo-package@spaces`.
//!
//! Everything Spaces *does* lives in the `leo-spaces` library one directory up
//! and is shared with the app shape. This file is only the translation into the
//! package contract:
//!
//! | app shape (`leo-spaces start` / `mcp`)       | hosted (this crate)                                   |
//! |----------------------------------------------|-------------------------------------------------------|
//! | settings read back over `/graph/v1/settings` | mirrored by the host, read with `ctx.get_setting`     |
//! | HTTP on 127.0.0.1:8530                       | the host's fd 3 socket; our router nested at `/api`   |
//! | `/leo/ui/descriptor`, `/leo/ui/data` routes  | [`LeoPackage::ui_descriptor`] / [`LeoPackage::ui_data`] |
//! | stdio MCP, a hand-added `mcp_servers` row    | two `Tool`s the host serves on fd 4                   |
//! | runs as the hub, may `sudo` its wrapper      | `leo-pkg-spaces`, `NoNewPrivileges` — remote host only |
//!
//! ## Tool names
//!
//! The app shape's tools reach the model as `mcp_spaces_spaces` and
//! `mcp_spaces_spaces_host` — the `mcp_<server>_<tool>` prefix the bridge puts on
//! every stdio/http MCP server. A hosted package's server is `transport: "unix"`
//! under `/run/leo/packages/`, which the hub treats as one of its own and does
//! not prefix (docs/package-process-model.md, "Converting used to rename
//! `web_fetch`…"). So hosted they are **`spaces`** and **`spaces_host`** — the
//! names this package's own schemas always used. Nothing in this repo names the
//! prefixed form; a saved prompt or agent note that does will need the bare name.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use leo_sdk_package::{
    Access, DataClass, DataFlow, FieldDef, FieldType, LeoPackage, PackageCategory, PackageContext,
    PackageManifest, PackagePage, PackageState,
};
use leo_sdk_tools::{RiskLevel, Tool, ToolDeadline, ToolDefinition, ToolResult};
use serde_json::Value;

use leo_spaces::config::{Config, SETTING_KEYS};
use leo_spaces::AppState;

/// The package. Stateless: the host (the hypervisor machine) is the source of
/// truth about guests, and the config is rebuilt from settings on every reload.
pub struct SpacesPackage;

/// The one page, as the app shape declares it.
const PAGE: &str = "spaces";

/// Read the five declared settings out of the host's mirror.
async fn settings(ctx: &PackageContext) -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    for key in SETTING_KEYS {
        if let Ok(Some(v)) = ctx.get_setting(key).await {
            m.insert(key.to_string(), v);
        }
    }
    m
}

async fn config(ctx: &PackageContext) -> Arc<Config> {
    Arc::new(Config::from_settings_map(&settings(ctx).await))
}

#[async_trait]
impl LeoPackage for SpacesPackage {
    fn manifest(&self) -> PackageManifest {
        PackageManifest {
            name: "spaces",
            label: "Spaces",
            description: "Run whole machines on a Leo hub — install the hypervisor, create \
                          Windows and Linux guests, and hand one a GPU.",
            details: "Drives a hypervisor host over SSH: reports what the box is — \
                      distribution, CPU, IOMMU, graphics devices — derives the remaining setup \
                      steps fresh on every load, and runs KVM/QEMU guests through libvirt: \
                      define one, start it, snapshot it, assign it a GPU exclusively, open its \
                      console. Windows guests get UEFI firmware, a software TPM and the virtio \
                      driver ISO without being asked. This is the hosted build: it runs as its \
                      own locked-down account and drives a REMOTE host with its own SSH key; to \
                      virtualise on the hub itself, install Spaces in app shape on that box.",
            setup_hint: "Set Host to the hypervisor machine and SSH user to an account there \
                         that can run `sudo -n /usr/local/bin/leo-spaces-admin`. Give the \
                         package its own key: `ssh-keygen -t ed25519 -N '' -f ./spaces_key`, \
                         then `leo packages service install-credential spaces ./spaces_key \
                         --name ssh_key`, and append spaces_key.pub to that account's \
                         ~/.ssh/authorized_keys on the host. Leave SSH key path blank (it \
                         defaults to /etc/leo-spaces/ssh_key) or set it to exactly that.",
            use_cases: &[
                "Turn a spare box into a hypervisor without a console",
                "Run Windows-only CAD and simulation software on Linux metal",
                "Give one guest the GPU outright, and see why another cannot have it",
                "Snapshot a machine before letting an agent loose on it",
            ],
            icon: "macwindow.on.rectangle",
            category: PackageCategory::Package,
            dependencies: &[],
        }
    }

    /// The same five keys, labels and types as the app shape's descriptor, so
    /// the values already stored on a hub carry across the conversion untouched
    /// (settings are keyed by name in the hub's `settings` table, not by shape).
    fn fields(&self) -> Vec<FieldDef> {
        let text = |key, label| FieldDef {
            key,
            label,
            field_type: FieldType::Text,
            required: false,
            options: None,
        };
        vec![
            text("spaces_host", "Host"),
            text("spaces_ssh_user", "SSH user (remote host only)"),
            FieldDef {
                key: "spaces_ssh_port",
                label: "SSH port (remote host only)",
                field_type: FieldType::Number,
                required: false,
                options: None,
            },
            text("spaces_ssh_key", "SSH key path (remote host only)"),
            text(
                "spaces_guest_hub",
                "Hub address guests pair with (blank: derived from the host)",
            ),
        ]
    }

    fn pages(&self) -> Vec<PackagePage> {
        vec![PackagePage {
            path: PAGE,
            label: "Spaces",
            icon: "macwindow.on.rectangle",
            sidebar_group: Some("Workspace"),
            settings_page: false,
            builtin_route: None,
            show_in_nav: true,
        }]
    }

    /// A console is the whole machine, and every write changes a host or a
    /// guest — the app shape's `access.mode: admin_only`, kept.
    fn access(&self) -> Access {
        Access::AdminOnly
    }

    fn default_enabled(&self) -> bool {
        false
    }

    /// The page tree — the same `ui::pages()` the app shape embeds in its
    /// descriptor, so the two shapes render one screen.
    fn ui_descriptor(&self) -> Option<Value> {
        Some(leo_spaces::ui::pages())
    }

    /// Everything live for the page.
    ///
    /// `is_admin` is passed as `true` and that is a reasoned choice, not a
    /// shortcut: the host's `/leo/ui/data` hands the package the user and drops
    /// the admin bit (`leo-package-host` `ui_data`, `let (user, _admin)`), and
    /// this package is `Access::AdminOnly`, so the hub's gate has refused every
    /// non-admin before the request reaches the socket. The flag only decides
    /// whether the action buttons render; every write is still checked against
    /// `x-leo-is-admin` by the REST route it calls (`auth::AdminCaller`).
    async fn ui_data(
        &self,
        ctx: &PackageContext,
        _user_id: &str,
        params: Value,
    ) -> anyhow::Result<Value> {
        match params.get("page").and_then(Value::as_str).unwrap_or(PAGE) {
            "" | PAGE => {
                let state = AppState {
                    cfg: config(ctx).await,
                };
                Ok(leo_spaces::ui::data(&state, true).await)
            }
            other => anyhow::bail!("spaces has no page '{other}' — it serves only '{PAGE}'"),
        }
    }

    async fn reload(&self, ctx: &PackageContext) -> anyhow::Result<PackageState> {
        // Before anything can run a host command: this process is the hosted
        // shape, so the local machine is refused by name (see
        // `leo_spaces::hosted`).
        leo_spaces::hosted::mark();
        let cfg = config(ctx).await;
        tracing_note(&cfg);
        let tools: Vec<Box<dyn Tool>> = leo_spaces::mcp::tool_schemas()
            .into_iter()
            .map(|schema| {
                Box::new(SpacesTool {
                    schema,
                    cfg: cfg.clone(),
                }) as Box<dyn Tool>
            })
            .collect();
        Ok(PackageState {
            tools,
            router: Some(leo_spaces::hosted_router(AppState { cfg })),
            ..Default::default()
        })
    }

    async fn test(&self, ctx: &PackageContext) -> (bool, String) {
        let cfg = config(ctx).await;
        let f = match leo_spaces::facts::probe(&cfg.host).await {
            Ok(f) => f,
            Err(e) => return (false, e.to_string()),
        };
        match (f.reachable, f.unreachable_reason) {
            (true, _) => (true, format!("reached {}", cfg.host)),
            (false, why) => (
                false,
                why.unwrap_or_else(|| format!("could not reach {}", cfg.host)),
            ),
        }
    }

    /// What leaves this process: commands, and for a Windows guest its product
    /// key and generated admin password, to the hypervisor host named in
    /// Settings, over SSH with the package's own key.
    fn data_disclosure(&self) -> Vec<DataFlow> {
        vec![DataFlow::to("The hypervisor host named in Settings (SSH)")
            .sends([DataClass::Credentials])
            .because(
                "Guests are defined and run on that machine; a Windows guest's product key \
                 and generated admin password are written into its unattended-install media \
                 there.",
            )]
    }

    fn icon_asset(&self) -> Option<leo_sdk_package::IconAsset> {
        leo_sdk_package::bundled_icon!(svg, "../assets/icon.svg")
    }
}

/// One line at reload saying which host this copy drives — the first thing an
/// operator reading `journalctl -u leo-package@spaces` needs.
fn tracing_note(cfg: &Config) {
    if cfg.host.is_local() {
        eprintln!(
            "leo-spaces (hosted): no Host set — {}",
            leo_spaces::hosted::LOCAL_REFUSED
        );
    } else {
        eprintln!("leo-spaces (hosted): driving {}", cfg.host);
    }
}

/// One of the two tools, from its MCP schema.
struct SpacesTool {
    schema: Value,
    cfg: Arc<Config>,
}

impl SpacesTool {
    fn name_str(&self) -> &str {
        self.schema["name"].as_str().unwrap_or_default()
    }
}

#[async_trait]
impl Tool for SpacesTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name_str().to_string(),
            description: self.schema["description"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            input_schema: self.schema["inputSchema"].clone(),
            // Both tools can delete guests and rewrite how a box boots.
            risk_level: RiskLevel::High,
        }
    }

    async fn execute(&self, input: Value) -> ToolResult {
        let (text, is_error) = leo_spaces::mcp::call(&self.cfg, self.name_str(), &input).await;
        if is_error {
            ToolResult::error(text)
        } else {
            ToolResult::success(text)
        }
    }

    /// `fetch_iso`, guest-tools builds and a Windows define run minutes, not
    /// seconds; the turn deadline (300 s) is the right ceiling for a tool the
    /// model waits on, and the long operations already return early and finish
    /// in the background on the host.
    fn deadline(&self) -> ToolDeadline {
        ToolDeadline::TURN
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The hosted tools are the two the app shape serves, by their bare names.
    #[test]
    fn the_tools_are_spaces_and_spaces_host() {
        let names: Vec<String> = leo_spaces::mcp::tool_schemas()
            .into_iter()
            .map(|schema| {
                SpacesTool {
                    schema,
                    cfg: Arc::new(Config::from_env()),
                }
                .definition()
                .name
            })
            .collect();
        assert_eq!(names, ["spaces", "spaces_host"]);
    }

    /// The fields are the five keys the config reads, in its order — a field
    /// nothing reads, or a setting nothing declares (so the host never
    /// mirrors it), is a silent loss.
    #[test]
    fn the_fields_are_the_settings_the_config_reads() {
        let keys: Vec<&str> = SpacesPackage.fields().iter().map(|f| f.key).collect();
        assert_eq!(keys, SETTING_KEYS);
    }

    /// The fields match the app shape's descriptor key for key and type for
    /// type, so the values a hub already stores carry across.
    #[test]
    fn the_fields_match_the_app_shapes_descriptor() {
        let d: Value = serde_json::from_str(include_str!("../../descriptor.json")).unwrap();
        let app: Vec<(String, String)> = d["fields"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| {
                (
                    f["key"].as_str().unwrap().to_string(),
                    f["field_type"].as_str().unwrap().to_string(),
                )
            })
            .collect();
        let hosted: Vec<(String, String)> = SpacesPackage
            .fields()
            .iter()
            .map(|f| {
                (
                    f.key.to_string(),
                    serde_json::to_value(f.field_type)
                        .unwrap()
                        .as_str()
                        .unwrap()
                        .to_string(),
                )
            })
            .collect();
        assert_eq!(hosted, app);
    }

    /// One screen for both shapes.
    #[test]
    fn the_page_tree_is_the_app_shapes() {
        let d: Value = serde_json::from_str(include_str!("../../descriptor.json")).unwrap();
        assert_eq!(SpacesPackage.ui_descriptor().as_ref(), Some(&d["ui"]));
        let pages: Vec<&str> = SpacesPackage.pages().iter().map(|p| p.path).collect();
        assert_eq!(pages, [PAGE]);
    }

    /// The pkgver is the package's version.
    #[test]
    fn the_host_carries_the_packages_version() {
        let root = include_str!("../../Cargo.toml");
        let v = root
            .lines()
            .find_map(|l| l.strip_prefix("version = \""))
            .and_then(|l| l.strip_suffix('"'))
            .expect("root Cargo.toml has a version");
        assert_eq!(v, env!("CARGO_PKG_VERSION"));
    }
}
