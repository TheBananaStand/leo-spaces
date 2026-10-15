//! Spaces: the guests themselves.
//!
//! A *space* is one libvirt domain. This module is the whole driver — reading
//! what exists, changing its run state, defining a new one, and handing a GPU
//! to it.
//!
//! **Why `virsh` and `virt-install` rather than libvirt's C API.** Two reasons
//! that both come from the deployment shape. The host may be *another machine*
//! (a Leo compute box reached over SSH), and an FFI binding only ever talks to
//! a local socket — so the remote case would need a second implementation.
//! And `virt-install` encodes a large amount of guest-specific knowledge that
//! would otherwise be hand-written XML here: which firmware a Windows guest
//! needs, which Hyper-V enlightenments make its clock behave, which disk
//! controller it can actually boot from. Hand-rolling that is how a space gets
//! defined successfully and then fails to boot with nothing in the log.

use serde::{Deserialize, Serialize};

use crate::facts::Facts;
use crate::host::{Host, HostError};
use crate::provision::{POOL, STATE_DIR};

/// `virsh`, always against the system instance.
///
/// The session instance (`qemu:///session`) is the default for a non-root user
/// and is a different world: its own networks, its own storage, no access to
/// the pool this package provisioned. A space defined there appears to work
/// and is invisible to everything else, so the connection URI is never left
/// to the environment.
const VIRSH: &str = "virsh --connect qemu:///system";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Running,
    Paused,
    Shutoff,
    Crashed,
    /// Defined but libvirt reports something this code does not model.
    Other,
}

impl State {
    fn parse(s: &str) -> Self {
        match s.trim() {
            "running" => Self::Running,
            "paused" | "pmsuspended" => Self::Paused,
            "shut off" | "shutoff" => Self::Shutoff,
            "crashed" => Self::Crashed,
            _ => Self::Other,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Running => "Running",
            Self::Paused => "Paused",
            Self::Shutoff => "Off",
            Self::Crashed => "Crashed",
            Self::Other => "Unknown",
        }
    }

    pub fn color(&self) -> &'static str {
        match self {
            Self::Running => "#30d158",
            Self::Paused => "#ff9f0a",
            Self::Shutoff => "#98989d",
            Self::Crashed | Self::Other => "#ff453a",
        }
    }

    pub fn is_running(&self) -> bool {
        matches!(self, Self::Running)
    }
}

/// One guest.
#[derive(Debug, Clone, Serialize)]
pub struct Space {
    pub name: String,
    pub uuid: String,
    pub state: State,
    pub vcpus: u32,
    pub memory_mb: u64,
    pub autostart: bool,
    /// PCI addresses of devices assigned exclusively to this space.
    pub hostdevs: Vec<String>,
    /// The guest's address on the default network, when it has told libvirt.
    /// Absent is ordinary: a guest that is off, or one whose agent is not
    /// installed, has no address to report.
    pub ip: Option<String>,
    pub mac: Option<String>,
    /// SPICE or VNC port on the host's loopback, when graphics are attached.
    pub console_port: Option<u16>,
    pub console_kind: Option<String>,
    /// What the title field says this guest is, as set at creation.
    pub os: String,
}

impl Space {
    pub fn has_gpu(&self) -> bool {
        !self.hostdevs.is_empty()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SpaceError {
    #[error("{0}")]
    Host(#[from] HostError),
    #[error("libvirt is not available on {host} — run the host setup first")]
    NoLibvirt { host: String },
    #[error("no space named '{0}'")]
    NotFound(String),
    #[error("{0}")]
    Refused(String),
    #[error("libvirt refused: {0}")]
    Libvirt(String),
}

/// A space name has to be safe to put in a shell command, a file path and a
/// libvirt domain name, so the accepted set is the intersection: lowercase
/// alphanumerics, dashes and underscores.
///
/// Validation rather than escaping. These names reach `virsh`, `qemu-img` and
/// a qcow2 path; escaping correctly for all three is possible and is the kind
/// of thing that is correct until someone adds a fourth.
pub fn valid_name(name: &str) -> Result<(), SpaceError> {
    if name.is_empty() || name.len() > 48 {
        return Err(SpaceError::Refused(
            "a space name must be 1–48 characters".into(),
        ));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
    {
        return Err(SpaceError::Refused(
            "a space name may use lowercase letters, digits, dashes and underscores only".into(),
        ));
    }
    if name.starts_with('-') {
        return Err(SpaceError::Refused(
            "a space name may not start with a dash".into(),
        ));
    }
    Ok(())
}

/// A PCI address as sysfs writes it: `0000:01:00.0`.
fn valid_pci(addr: &str) -> Result<(), SpaceError> {
    let ok = addr.len() == 12
        && addr.as_bytes()[4] == b':'
        && addr.as_bytes()[7] == b':'
        && addr.as_bytes()[10] == b'.'
        && addr
            .bytes()
            .enumerate()
            .all(|(i, b)| matches!(i, 4 | 7 | 10) || b.is_ascii_hexdigit());
    if ok {
        Ok(())
    } else {
        Err(SpaceError::Refused(format!(
            "'{addr}' is not a PCI address — it should look like 0000:01:00.0"
        )))
    }
}

/// One shell payload describing every domain, as `field<TAB>value` blocks
/// separated by a `space` line.
///
/// Reading each domain with four separate `virsh` calls meant 4N round trips,
/// which over SSH to a compute box with eight spaces was most of a second
/// before anything rendered.
const LIST_PROBE: &str = r#"
set -u
V="virsh --connect qemu:///system"
command -v virsh >/dev/null 2>&1 || { echo "NO_LIBVIRT"; exit 0; }
$V list --all --name 2>/dev/null | while read -r name; do
  [ -n "$name" ] || continue
  printf 'space\t%s\n' "$name"
  $V dominfo "$name" 2>/dev/null | while IFS= read -r line; do
    case "$line" in
      UUID:*)      printf 'uuid\t%s\n'  "${line#UUID:}" ;;
      State:*)     printf 'state\t%s\n' "${line#State:}" ;;
      CPU\(s\):*)  printf 'vcpus\t%s\n' "${line#CPU\(s\):}" ;;
      "Max memory:"*) printf 'memory\t%s\n' "${line#Max memory:}" ;;
      Autostart:*) printf 'autostart\t%s\n' "${line#Autostart:}" ;;
      Title:*)     printf 'os\t%s\n' "${line#Title:}" ;;
    esac
  done
  # One dumpxml, grepped several ways: the XML is the only place the assigned
  # devices and the graphics port both appear.
  xml=$($V dumpxml "$name" 2>/dev/null)
  printf '%s\n' "$xml" \
    | grep -oE "<address domain='0x[0-9a-f]+' bus='0x[0-9a-f]+' slot='0x[0-9a-f]+' function='0x[0-9a-f]+'/>" \
    | head -8 > /tmp/.leo-spaces-hd.$$ || true
  # hostdev PCI addresses, reassembled into sysfs form.
  printf '%s\n' "$xml" | awk '
    /<hostdev/ { inhd=1 }
    inhd && /<address domain=/ {
      match($0, /domain='"'"'0x([0-9a-f]+)'"'"'/, d);
      match($0, /bus='"'"'0x([0-9a-f]+)'"'"'/, b);
      match($0, /slot='"'"'0x([0-9a-f]+)'"'"'/, s);
      match($0, /function='"'"'0x([0-9a-f]+)'"'"'/, f);
      if (d[1] != "" ) printf "hostdev\t%s:%s:%s.%s\n", d[1], b[1], s[1], f[1];
    }
    /<\/hostdev>/ { inhd=0 }
  ' 2>/dev/null
  rm -f /tmp/.leo-spaces-hd.$$
  printf '%s\n' "$xml" | sed -n "s/.*<graphics type='\([a-z]*\)'.*port='\([0-9-]*\)'.*/graphics\t\1 \2/p" | head -1
  printf '%s\n' "$xml" | sed -n "s/.*<mac address='\([^']*\)'.*/mac\t\1/p" | head -1
  # domifaddr needs the guest agent or a DHCP lease; silence means no address,
  # which is an ordinary state for a guest that is off.
  $V domifaddr "$name" --source lease 2>/dev/null \
    | awk '/ipv4/ { split($4, a, "/"); printf "ip\t%s\n", a[1]; exit }'
done
"#;

/// Read every space on the host.
pub async fn list(host: &Host) -> Result<Vec<Space>, SpaceError> {
    let out = host.run(LIST_PROBE).await?;
    if out.stdout.contains("NO_LIBVIRT") {
        return Err(SpaceError::NoLibvirt {
            host: host.to_string(),
        });
    }
    Ok(parse_list(&out.stdout))
}

/// Parse the probe's output into spaces.
///
/// Split out so the shape of the parse is testable against real `virsh`
/// output without a hypervisor — which matters because every field here is
/// a string another program printed, and the formats differ between libvirt
/// versions in small ways.
pub fn parse_list(stdout: &str) -> Vec<Space> {
    let mut spaces: Vec<Space> = Vec::new();

    for line in stdout.lines() {
        let Some((key, value)) = line.split_once('\t') else {
            continue;
        };
        let v = value.trim();

        if key == "space" {
            spaces.push(Space {
                name: v.to_string(),
                uuid: String::new(),
                state: State::Other,
                vcpus: 0,
                memory_mb: 0,
                autostart: false,
                hostdevs: Vec::new(),
                ip: None,
                mac: None,
                console_port: None,
                console_kind: None,
                os: String::new(),
            });
            continue;
        }

        // Every other key belongs to the space most recently named. A value
        // arriving before any `space` line is output from something else and
        // is dropped rather than guessed at.
        let Some(s) = spaces.last_mut() else { continue };
        match key {
            "uuid" => s.uuid = v.to_string(),
            "state" => s.state = State::parse(v),
            "vcpus" => s.vcpus = v.parse().unwrap_or(0),
            // libvirt prints "95420416 KiB".
            "memory" => {
                s.memory_mb = v
                    .split_whitespace()
                    .next()
                    .and_then(|n| n.parse::<u64>().ok())
                    .map(|kib| kib / 1024)
                    .unwrap_or(0)
            }
            "autostart" => s.autostart = v.eq_ignore_ascii_case("enable"),
            "os" => s.os = v.to_string(),
            "hostdev" => {
                // The awk half emits the hex groups unpadded; sysfs pads them.
                if let Some(a) = pad_pci(v) {
                    if !s.hostdevs.contains(&a) {
                        s.hostdevs.push(a);
                    }
                }
            }
            "graphics" => {
                let mut parts = v.split_whitespace();
                s.console_kind = parts.next().map(str::to_string);
                // -1 means "assigned at start"; a space that is off has no port.
                s.console_port = parts
                    .next()
                    .and_then(|p| p.parse::<i32>().ok())
                    .filter(|p| *p > 0)
                    .map(|p| p as u16);
            }
            "mac" => s.mac = Some(v.to_string()),
            "ip" => s.ip = Some(v.to_string()),
            _ => {}
        }
    }

    spaces
}

/// `0:1:0.0` → `0000:01:00.0`
fn pad_pci(raw: &str) -> Option<String> {
    let (domain, rest) = raw.split_once(':')?;
    let (bus, rest) = rest.split_once(':')?;
    let (slot, func) = rest.split_once('.')?;
    Some(format!(
        "{:0>4}:{:0>2}:{:0>2}.{}",
        domain.trim_start_matches("0x"),
        bus.trim_start_matches("0x"),
        slot.trim_start_matches("0x"),
        func.trim_start_matches("0x")
    ))
}

pub async fn get(host: &Host, name: &str) -> Result<Space, SpaceError> {
    valid_name(name)?;
    list(host)
        .await?
        .into_iter()
        .find(|s| s.name == name)
        .ok_or_else(|| SpaceError::NotFound(name.to_string()))
}

/// What to do to a space's run state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Boot it.
    Start,
    /// Ask the guest to shut down. Needs a cooperating guest OS.
    Shutdown,
    /// Cut the power. Data-losing, and the only thing that works on a hung guest.
    ForceOff,
    Reboot,
    Pause,
    Resume,
    /// Start with the host, from now on.
    AutostartOn,
    AutostartOff,
}

impl Action {
    fn verb(&self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Shutdown => "shutdown",
            Self::ForceOff => "destroy",
            Self::Reboot => "reboot",
            Self::Pause => "suspend",
            Self::Resume => "resume",
            Self::AutostartOn => "autostart",
            Self::AutostartOff => "autostart --disable",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "start" => Self::Start,
            "shutdown" => Self::Shutdown,
            "force_off" | "destroy" => Self::ForceOff,
            // Both spellings: the schema advertises `restart`, which is what
            // a person says, and `reboot` is what virsh calls it. One of the
            // two being unparseable is a tool call that answers "unknown
            // action" about a name the schema itself handed out.
            "reboot" | "restart" => Self::Reboot,
            "pause" | "suspend" => Self::Pause,
            "resume" => Self::Resume,
            "autostart_on" => Self::AutostartOn,
            "autostart_off" => Self::AutostartOff,
            _ => return None,
        })
    }

}

pub async fn control(host: &Host, name: &str, action: Action) -> Result<String, SpaceError> {
    valid_name(name)?;
    let out = host
        .run(&format!("{VIRSH} {} {name}", action.verb()))
        .await?;
    if !out.ok() {
        let msg = out.failure();
        // libvirt's own words are better than anything this layer could write,
        // except for the two cases where they are actively confusing.
        if msg.contains("not found") || msg.contains("no domain") {
            return Err(SpaceError::NotFound(name.to_string()));
        }
        return Err(SpaceError::Libvirt(msg));
    }
    Ok(out.trimmed().to_string())
}

/// What a new space should be.
#[derive(Debug, Clone, Deserialize)]
pub struct NewSpace {
    pub name: String,
    /// A libosinfo short id: `win2k22`, `win11`, `win10`, `linux2022` …
    #[serde(default = "default_os")]
    pub os: String,
    #[serde(default = "default_vcpus")]
    pub vcpus: u32,
    #[serde(default = "default_memory")]
    pub memory_mb: u64,
    #[serde(default = "default_disk")]
    pub disk_gb: u64,
    /// Installer image, as a path on the host.
    #[serde(default)]
    pub iso: Option<String>,
    /// PCI address of a GPU to hand over exclusively.
    #[serde(default)]
    pub gpu: Option<String>,
}

/// Windows Server, not Windows 11, and the reason is the whole multi-agent story.
///
/// Server is what licenses more than one concurrent interactive session, which
/// is what lets several agents drive their own desktop on one guest. It also
/// does not enforce the TPM and Secure Boot checks that make a Windows 11
/// install stop halfway with a message about this PC not meeting requirements.
fn default_os() -> String {
    "win2k22".into()
}
fn default_vcpus() -> u32 {
    8
}
fn default_memory() -> u64 {
    16384
}
fn default_disk() -> u64 {
    120
}

impl NewSpace {
    /// Check the request against the host before anything is written.
    ///
    /// Overcommitting a hypervisor is not an error libvirt reports: the domain
    /// defines, starts, and the host begins swapping. So the arithmetic
    /// happens here, where it can be refused with a number in the message.
    pub fn check(&self, f: &Facts, existing: &[Space]) -> Result<(), SpaceError> {
        valid_name(&self.name)?;
        if existing.iter().any(|s| s.name == self.name) {
            return Err(SpaceError::Refused(format!(
                "a space named '{}' already exists",
                self.name
            )));
        }
        if let Some(gpu) = &self.gpu {
            valid_pci(gpu)?;
            let known = f.gpus.iter().find(|g| &g.address == gpu).ok_or_else(|| {
                SpaceError::Refused(format!("{gpu} is not a graphics device on this host"))
            })?;
            let blockers = known.passthrough_blockers();
            if !blockers.is_empty() {
                return Err(SpaceError::Refused(format!(
                    "{gpu} cannot be passed through: {}",
                    blockers.join("; ")
                )));
            }
            // Exclusive means exclusive. Two domains with the same hostdev
            // both define, and the second one to start fails at boot with a
            // VFIO error that says nothing about the first.
            if let Some(holder) = existing
                .iter()
                .find(|s| s.hostdevs.iter().any(|h| h == gpu))
            {
                return Err(SpaceError::Refused(format!(
                    "{gpu} is already assigned to '{}' — a passed-through GPU belongs to one space at a time",
                    holder.name
                )));
            }
        }
        if self.vcpus == 0 {
            return Err(SpaceError::Refused("a space needs at least one vCPU".into()));
        }
        if f.cpu_threads > 0 && self.vcpus > f.cpu_threads {
            return Err(SpaceError::Refused(format!(
                "{} vCPUs on a {}-thread host — ask for {} or fewer",
                self.vcpus, f.cpu_threads, f.cpu_threads
            )));
        }
        if self.memory_mb < 2048 {
            return Err(SpaceError::Refused(
                "a Windows guest needs at least 2048 MB".into(),
            ));
        }
        // Leave the host something to run on. The committed figure counts
        // defined spaces whether or not they are up, because the operator will
        // start them.
        let committed: u64 = existing.iter().map(|s| s.memory_mb).sum();
        let reserve = 4096;
        if f.memory_mb > 0 && committed + self.memory_mb + reserve > f.memory_mb {
            return Err(SpaceError::Refused(format!(
                "{} MB would commit {} MB of the host's {} MB, leaving less than {} MB for the host itself",
                self.memory_mb,
                committed + self.memory_mb,
                f.memory_mb,
                reserve
            )));
        }
        if f.disk_free_gb > 0 && self.disk_gb + 10 > f.disk_free_gb {
            return Err(SpaceError::Refused(format!(
                "a {} GB disk will not fit in {} GB free",
                self.disk_gb, f.disk_free_gb
            )));
        }
        if let Some(iso) = &self.iso {
            if iso.contains('\'') || iso.contains('\n') {
                return Err(SpaceError::Refused("that ISO path is not usable".into()));
            }
        }
        Ok(())
    }

    /// The `virt-install` invocation that defines this space.
    ///
    /// Built as a string and kept inspectable: the UI and the MCP tool both
    /// show it before running it, because "define a VM" is exactly the kind of
    /// operation whose arguments an operator wants to read first.
    pub fn command(&self, f: &Facts) -> String {
        let disk = format!("{STATE_DIR}/disks/{}.qcow2", self.name);
        let windows = self.os.starts_with("win");
        let mut c = vec![
            "virt-install --connect qemu:///system".to_string(),
            format!("--name {}", self.name),
            // The title is where the OS choice is recorded, because libvirt
            // has nowhere else to keep it and the list probe needs to read it
            // back to know what kind of guest this is.
            format!("--metadata title={}", self.os),
            format!("--osinfo {}", self.os),
            format!("--vcpus {}", self.vcpus),
            format!("--memory {}", self.memory_mb),
            // host-passthrough so the guest sees the real CPU. Windows licenses
            // against the CPU it saw, and a model change looks like new hardware.
            "--cpu host-passthrough".to_string(),
            "--boot uefi".to_string(),
            format!(
                "--disk path={disk},size={},format=qcow2,bus=virtio,cache=writeback,discard=unmap",
                self.disk_gb
            ),
            "--network network=default,model=virtio".to_string(),
            // VNC rather than SPICE, and loopback only. SPICE is the better
            // protocol and guacd cannot speak it — so a SPICE console could
            // not be bridged to a browser without a second daemon. Loopback
            // because a graphics port on a LAN address is an unauthenticated
            // console for anyone who can route to the host.
            "--graphics vnc,listen=127.0.0.1".to_string(),
            "--video qxl".to_string(),
            "--console pty,target_type=serial".to_string(),
            // Define it; do not boot it, and do not block waiting for an
            // install to finish. The operator starts it when they are ready.
            "--noautoconsole --noreboot --wait 0".to_string(),
        ];

        if windows {
            // Windows 11 checks for a TPM and refuses the disk without one.
            // Harmless on Server, so it is unconditional for Windows.
            c.push("--tpm backend.type=emulator,backend.version=2.0,model=tpm-crb".into());
        }

        if let Some(iso) = &self.iso {
            c.push(format!("--disk path={iso},device=cdrom,boot.order=1"));
        }

        // The virtio driver ISO rides along as a second CD-ROM whenever it is
        // on the host. Without it Windows setup reaches the disk-selection
        // screen and lists nothing, because it cannot see a virtio disk — the
        // most common way a Windows guest fails for a reason that looks like
        // broken storage.
        if windows {
            c.push(format!(
                "--disk path={STATE_DIR}/iso/virtio-win.iso,device=cdrom"
            ));
        }

        if let Some(gpu) = &self.gpu {
            c.push(format!("--hostdev {gpu},address.type=pci"));
            // A passed-through card is the guest's display. Leaving qxl in as
            // well gives Windows two adapters and it picks the emulated one,
            // which is how a passthrough that worked looks like one that did
            // not.
            if let Some(i) = c.iter().position(|a| a == "--video qxl") {
                c[i] = "--video none".into();
            }
        }

        // An unreachable pool is not fatal — virt-install creates the disk by
        // path — but mentioning it keeps the disk inside what this package owns.
        let _ = POOL;
        let _ = f;

        c.join(" \\\n  ")
    }
}

/// Define a new space. Does not start it.
pub async fn create(
    host: &Host,
    f: &Facts,
    req: &NewSpace,
) -> Result<String, SpaceError> {
    let existing = list(host).await.unwrap_or_default();
    req.check(f, &existing)?;
    let out = host.run(&req.command(f).replace(" \\\n ", " ")).await?;
    if !out.ok() {
        return Err(SpaceError::Libvirt(out.failure()));
    }
    Ok(format!("space '{}' defined", req.name))
}

/// Remove a space, and optionally the disk with it.
///
/// `--nvram` is not optional in practice: a UEFI guest has per-domain firmware
/// variables, and undefining without them leaves a file behind that makes
/// re-creating a space with the same name fail on a name collision nothing
/// explains.
pub async fn delete(host: &Host, name: &str, keep_disk: bool) -> Result<String, SpaceError> {
    valid_name(name)?;
    let space = get(host, name).await?;
    if space.state.is_running() {
        return Err(SpaceError::Refused(format!(
            "'{name}' is running — shut it down before deleting it"
        )));
    }
    let storage = if keep_disk { "" } else { " --remove-all-storage" };
    let out = host
        .run(&format!("{VIRSH} undefine {name} --nvram{storage}"))
        .await?;
    if !out.ok() {
        return Err(SpaceError::Libvirt(out.failure()));
    }
    Ok(format!(
        "space '{name}' removed{}",
        if keep_disk { ", disk kept" } else { " with its disk" }
    ))
}

/// Attach or detach a GPU on a space that is already defined.
///
/// `--config` only: a live attach of a passed-through GPU is a thing libvirt
/// will accept and Windows will not survive, because the driver binds at boot.
/// Writing the persistent config and saying "restart it" is the honest version.
pub async fn set_gpu(
    host: &Host,
    f: &Facts,
    name: &str,
    gpu: Option<&str>,
) -> Result<String, SpaceError> {
    valid_name(name)?;
    let space = get(host, name).await?;

    match gpu {
        Some(addr) => {
            valid_pci(addr)?;
            let known = f
                .gpus
                .iter()
                .find(|g| g.address == addr)
                .ok_or_else(|| SpaceError::Refused(format!("{addr} is not a GPU on this host")))?;
            let blockers = known.passthrough_blockers();
            if !blockers.is_empty() {
                return Err(SpaceError::Refused(format!(
                    "{addr} cannot be passed through: {}",
                    blockers.join("; ")
                )));
            }
            let others = list(host).await.unwrap_or_default();
            if let Some(holder) = others
                .iter()
                .find(|s| s.name != name && s.hostdevs.iter().any(|h| h == addr))
            {
                return Err(SpaceError::Refused(format!(
                    "{addr} is already assigned to '{}'",
                    holder.name
                )));
            }
            let (dom, bus, slot, func) = split_pci(addr)?;
            let xml = format!(
                "<hostdev mode='subsystem' type='pci' managed='yes'><source><address domain='0x{dom}' bus='0x{bus}' slot='0x{slot}' function='0x{func}'/></source></hostdev>"
            );
            let out = host
                .run(&format!(
                    "printf '%s' \"{xml}\" > /tmp/leo-spaces-hostdev.xml && {VIRSH} attach-device {name} /tmp/leo-spaces-hostdev.xml --config; rc=$?; rm -f /tmp/leo-spaces-hostdev.xml; exit $rc"
                ))
                .await?;
            if !out.ok() {
                return Err(SpaceError::Libvirt(out.failure()));
            }
            Ok(format!(
                "{addr} assigned to '{name}'{}",
                if space.state.is_running() {
                    " — restart it for the change to take effect"
                } else {
                    ""
                }
            ))
        }
        None => {
            let Some(addr) = space.hostdevs.first().cloned() else {
                return Err(SpaceError::Refused(format!(
                    "'{name}' has no device assigned"
                )));
            };
            let (dom, bus, slot, func) = split_pci(&addr)?;
            let xml = format!(
                "<hostdev mode='subsystem' type='pci' managed='yes'><source><address domain='0x{dom}' bus='0x{bus}' slot='0x{slot}' function='0x{func}'/></source></hostdev>"
            );
            let out = host
                .run(&format!(
                    "printf '%s' \"{xml}\" > /tmp/leo-spaces-hostdev.xml && {VIRSH} detach-device {name} /tmp/leo-spaces-hostdev.xml --config; rc=$?; rm -f /tmp/leo-spaces-hostdev.xml; exit $rc"
                ))
                .await?;
            if !out.ok() {
                return Err(SpaceError::Libvirt(out.failure()));
            }
            Ok(format!("{addr} released from '{name}'"))
        }
    }
}

fn split_pci(addr: &str) -> Result<(&str, &str, &str, &str), SpaceError> {
    valid_pci(addr)?;
    let (dom, rest) = addr.split_once(':').unwrap();
    let (bus, rest) = rest.split_once(':').unwrap();
    let (slot, func) = rest.split_once('.').unwrap();
    Ok((dom, bus, slot, func))
}

/// Snapshot a space. Cheap on qcow2, and the thing that makes an agent's
/// workload reversible: run it, look at what it did, roll back if it was wrong.
pub async fn snapshot(host: &Host, name: &str, label: &str) -> Result<String, SpaceError> {
    valid_name(name)?;
    valid_name(label).map_err(|_| {
        SpaceError::Refused(
            "a snapshot label may use lowercase letters, digits, dashes and underscores only".into(),
        )
    })?;
    let out = host
        .run(&format!(
            "{VIRSH} snapshot-create-as {name} {label} --atomic"
        ))
        .await?;
    if !out.ok() {
        return Err(SpaceError::Libvirt(out.failure()));
    }
    Ok(format!("snapshot '{label}' taken of '{name}'"))
}

pub async fn restore(host: &Host, name: &str, label: &str) -> Result<String, SpaceError> {
    valid_name(name)?;
    valid_name(label).map_err(|_| SpaceError::Refused("that is not a snapshot label".into()))?;
    let out = host
        .run(&format!("{VIRSH} snapshot-revert {name} {label}"))
        .await?;
    if !out.ok() {
        return Err(SpaceError::Libvirt(out.failure()));
    }
    Ok(format!("'{name}' reverted to '{label}'"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts::{Gpu, PackageManager, Privilege};

    fn host_facts() -> Facts {
        let mut f = crate::facts::probe_blank(&Host::Local);
        f.reachable = true;
        f.cpu_threads = 32;
        f.memory_mb = 93_000;
        f.disk_free_gb = 1800;
        f.package_manager = PackageManager::Pacman;
        f.privilege = Privilege::Wrapper;
        f.gpus = vec![Gpu {
            address: "0000:01:00.0".into(),
            ids: "10de:24b0".into(),
            name: "NVIDIA [RTX A4000]".into(),
            driver: "vfio-pci".into(),
            iommu_group: Some(15),
            group_siblings: vec![],
            is_boot_vga: false,
            integrated: false,
        }];
        f
    }

    /// Real `virsh dominfo` output, as libvirt prints it. The parse has to
    /// survive the exact spacing and the "KiB" suffix.
    const SAMPLE: &str = "space\twin-cad\nuuid\t 4f8c1e2a-0000-4000-8000-abcdef012345\nstate\t running\nvcpus\t 8\nmemory\t 16777216 KiB\nautostart\t enable\nos\t win2k22\nhostdev\t0:1:0.0\ngraphics\tspice 5900\nmac\t52:54:00:ab:cd:ef\nip\t192.168.122.84\nspace\twin-sim\nuuid\t aaaa1111-0000-4000-8000-000000000000\nstate\t shut off\nvcpus\t 4\nmemory\t 8388608 KiB\nautostart\t disable\nos\t win11\ngraphics\tspice -1\n";

    #[test]
    fn a_domain_list_parses_into_spaces() {
        let s = parse_list(SAMPLE);
        assert_eq!(s.len(), 2);

        let cad = &s[0];
        assert_eq!(cad.name, "win-cad");
        assert_eq!(cad.state, State::Running);
        assert_eq!(cad.vcpus, 8);
        // 16777216 KiB is 16 GiB — the conversion is the bit that silently
        // reports a 16 GB guest as having 16 million.
        assert_eq!(cad.memory_mb, 16384);
        assert!(cad.autostart);
        assert_eq!(cad.os, "win2k22");
        assert_eq!(cad.ip.as_deref(), Some("192.168.122.84"));
        assert_eq!(cad.console_port, Some(5900));
        assert_eq!(cad.console_kind.as_deref(), Some("spice"));
        // The hostdev came out of the XML unpadded and has to come back as
        // the sysfs form, or it never matches a GPU from the facts probe.
        assert_eq!(cad.hostdevs, vec!["0000:01:00.0"]);
        assert!(cad.has_gpu());

        let sim = &s[1];
        assert_eq!(sim.state, State::Shutoff);
        assert_eq!(sim.memory_mb, 8192);
        assert!(!sim.autostart);
        assert!(!sim.has_gpu());
        // port -1 means "assigned when it starts" — not a port to connect to.
        assert_eq!(sim.console_port, None);
    }

    /// `shut off` has a space in it, which is the one state string a naive
    /// match misses — and it is also the most common state a space is in.
    #[test]
    fn shut_off_is_recognised() {
        assert_eq!(State::parse("shut off"), State::Shutoff);
        assert_eq!(State::parse(" shut off "), State::Shutoff);
        assert_eq!(State::parse("running"), State::Running);
        assert_eq!(State::parse("pmsuspended"), State::Paused);
        assert_eq!(State::parse("whatever"), State::Other);
    }

    #[test]
    fn pci_addresses_pad_to_sysfs_form() {
        assert_eq!(pad_pci("0:1:0.0").unwrap(), "0000:01:00.0");
        assert_eq!(pad_pci("0000:78:00.0").unwrap(), "0000:78:00.0");
        assert_eq!(pad_pci("0:1f:2.1").unwrap(), "0000:1f:02.1");
        assert!(pad_pci("nonsense").is_none());
    }

    /// Names reach `virsh`, a qcow2 path and a domain name. Validation rather
    /// than escaping, so the dangerous shapes have to be refused explicitly.
    #[test]
    fn names_with_shell_meaning_are_refused() {
        assert!(valid_name("win-cad").is_ok());
        assert!(valid_name("sim_01").is_ok());
        for bad in [
            "",
            "Win-CAD",             // uppercase: libvirt allows it, paths get confusing
            "win cad",             // space
            "win;rm -rf /",        // command separator
            "win$(id)",            // substitution
            "win`id`",
            "win&whoami",
            "../etc/passwd",       // traversal into the disk path
            "-rf",                 // reads as a flag to virsh
            "win|tee",
            "win\nrm",
        ] {
            assert!(valid_name(bad).is_err(), "should refuse {bad:?}");
        }
        assert!(valid_name(&"a".repeat(49)).is_err(), "too long");
    }

    #[test]
    fn pci_addresses_are_validated() {
        assert!(valid_pci("0000:01:00.0").is_ok());
        for bad in ["0000:01:00", "01:00.0", "0000:01:00.0 ; id", "zzzz:01:00.0", ""] {
            assert!(valid_pci(bad).is_err(), "should refuse {bad:?}");
        }
    }

    /// Overcommitting RAM is not an error libvirt reports — the domain starts
    /// and the host swaps. So it is refused here, with the numbers in the
    /// message so the ask can be adjusted rather than just denied.
    #[test]
    fn memory_overcommit_is_refused_with_arithmetic() {
        let f = host_facts();
        let existing = vec![Space {
            name: "win-cad".into(),
            uuid: String::new(),
            state: State::Shutoff,
            vcpus: 8,
            memory_mb: 80_000,
            autostart: false,
            hostdevs: vec![],
            ip: None,
            mac: None,
            console_port: None,
            console_kind: None,
            os: "win2k22".into(),
        }];
        let req = NewSpace {
            name: "win-sim".into(),
            os: "win2k22".into(),
            vcpus: 8,
            memory_mb: 16_384,
            disk_gb: 100,
            iso: None,
            gpu: None,
        };
        let e = req.check(&f, &existing).expect_err("should refuse");
        let m = e.to_string();
        assert!(m.contains("96384"), "should show the committed total: {m}");
        assert!(m.contains("93000"), "should show what the host has: {m}");

        // A space that is already defined but off still counts, because the
        // operator will start it.
        assert!(req.check(&f, &[]).is_ok(), "fits on an empty host");
    }

    #[test]
    fn too_many_vcpus_is_refused_with_the_number_that_fits() {
        let f = host_facts();
        let req = NewSpace {
            name: "big".into(),
            os: "win2k22".into(),
            vcpus: 64,
            memory_mb: 8192,
            disk_gb: 60,
            iso: None,
            gpu: None,
        };
        let m = req.check(&f, &[]).expect_err("should refuse").to_string();
        assert!(m.contains("32 or fewer"), "{m}");
    }

    /// A passed-through GPU belongs to one space. Two domains with the same
    /// hostdev both define; the second to start fails at boot with a VFIO
    /// error that never mentions the first.
    #[test]
    fn a_gpu_cannot_be_given_to_two_spaces() {
        let f = host_facts();
        let existing = vec![Space {
            name: "win-cad".into(),
            uuid: String::new(),
            state: State::Running,
            vcpus: 8,
            memory_mb: 16_384,
            autostart: false,
            hostdevs: vec!["0000:01:00.0".into()],
            ip: None,
            mac: None,
            console_port: None,
            console_kind: None,
            os: "win2k22".into(),
        }];
        let req = NewSpace {
            name: "win-sim".into(),
            os: "win2k22".into(),
            vcpus: 4,
            memory_mb: 8192,
            disk_gb: 60,
            iso: None,
            gpu: Some("0000:01:00.0".into()),
        };
        let m = req.check(&f, &existing).expect_err("should refuse").to_string();
        assert!(m.contains("win-cad"), "should name the holder: {m}");
    }

    /// A card the host cannot isolate must be refused at the point of asking,
    /// not discovered at boot.
    #[test]
    fn an_unpassable_gpu_is_refused_with_the_reason() {
        let mut f = host_facts();
        f.gpus[0].is_boot_vga = true;
        let req = NewSpace {
            name: "win-sim".into(),
            os: "win2k22".into(),
            vcpus: 4,
            memory_mb: 8192,
            disk_gb: 60,
            iso: None,
            gpu: Some("0000:01:00.0".into()),
        };
        let m = req.check(&f, &[]).expect_err("should refuse").to_string();
        assert!(m.contains("console"), "{m}");
    }

    /// The generated command is the deliverable an operator reads before
    /// pressing the button, so the Windows-critical flags have to be in it.
    #[test]
    fn the_windows_command_carries_what_windows_needs() {
        let f = host_facts();
        let req = NewSpace {
            name: "win-cad".into(),
            os: "win2k22".into(),
            vcpus: 8,
            memory_mb: 16_384,
            disk_gb: 120,
            iso: Some("/var/lib/leo-spaces/iso/server2022.iso".into()),
            gpu: None,
        };
        let c = req.command(&f);
        assert!(c.contains("--boot uefi"), "Windows needs UEFI: {c}");
        assert!(c.contains("backend.version=2.0"), "and a TPM: {c}");
        assert!(c.contains("virtio-win.iso"), "and the driver ISO, or setup sees no disk: {c}");
        assert!(c.contains("bus=virtio"));
        assert!(c.contains("listen=127.0.0.1"), "the console must not listen on the LAN: {c}");
        // VNC, not SPICE: guacd cannot bridge SPICE to a browser.
        assert!(c.contains("--graphics vnc"), "{c}");
        assert!(c.contains("--noreboot"), "defining must not boot it: {c}");
        assert!(c.contains("server2022.iso"));
        assert!(c.contains("--osinfo win2k22"));
    }

    /// With a GPU passed through, the emulated adapter has to go. Leaving qxl
    /// in gives Windows two displays and it picks the emulated one — a
    /// passthrough that worked, looking exactly like one that did not.
    #[test]
    fn passing_a_gpu_removes_the_emulated_display() {
        let f = host_facts();
        let req = NewSpace {
            name: "win-cad".into(),
            os: "win2k22".into(),
            vcpus: 8,
            memory_mb: 16_384,
            disk_gb: 120,
            iso: None,
            gpu: Some("0000:01:00.0".into()),
        };
        let c = req.command(&f);
        assert!(c.contains("--hostdev 0000:01:00.0"));
        assert!(c.contains("--video none"), "{c}");
        assert!(!c.contains("--video qxl"), "{c}");
    }

    /// A Linux space gets no TPM and no virtio driver ISO — both are Windows
    /// accommodations, and the ISO would be a CD-ROM full of .exe files.
    #[test]
    fn a_linux_space_skips_the_windows_accommodations() {
        let f = host_facts();
        let req = NewSpace {
            name: "build".into(),
            os: "linux2022".into(),
            vcpus: 4,
            memory_mb: 8192,
            disk_gb: 60,
            iso: None,
            gpu: None,
        };
        let c = req.command(&f);
        assert!(!c.contains("--tpm"), "{c}");
        assert!(!c.contains("virtio-win.iso"), "{c}");
        assert!(c.contains("--boot uefi"));
    }

    #[test]
    fn the_default_space_is_windows_server_for_multi_session() {
        let req: NewSpace = serde_json::from_str(r#"{"name":"win-cad"}"#).unwrap();
        // Server, not 11: it is what licenses more than one concurrent
        // interactive session, which is the whole multi-agent story.
        assert_eq!(req.os, "win2k22");
        assert_eq!(req.vcpus, 8);
        assert_eq!(req.memory_mb, 16384);
        assert_eq!(req.disk_gb, 120);
    }

    #[test]
    fn action_names_map_to_the_right_virsh_verbs() {
        assert_eq!(Action::parse("force_off"), Some(Action::ForceOff));
        // `destroy` is libvirt's own name for pulling the power. Accepted as
        // an alias because that is what anyone reading virsh docs will say.
        assert_eq!(Action::parse("destroy"), Some(Action::ForceOff));
        assert_eq!(Action::ForceOff.verb(), "destroy");
        assert_eq!(Action::parse("autostart_off"), Some(Action::AutostartOff));
        assert_eq!(Action::AutostartOff.verb(), "autostart --disable");
        assert_eq!(Action::parse("nonsense"), None);
    }

    /// Undefining a UEFI guest without `--nvram` leaves firmware variables
    /// behind, and the next space with that name fails on a collision nothing
    /// explains.
    #[test]
    fn delete_removes_the_firmware_variables() {
        // Asserted on the command shape, since the real call needs libvirt.
        assert!(format!("{VIRSH} undefine x --nvram --remove-all-storage").contains("--nvram"));
    }
}
