//! What the host actually is, read once per request that needs it.
//!
//! Everything the provisioner decides comes from here, and nothing here
//! decides anything: `Facts` is a photograph. The split matters because the
//! interesting bugs in host provisioning are all "we assumed X was installed"
//! — and an assumption is only visible when the thing it stands in for is a
//! named field somebody can print.
//!
//! One shell round-trip fills the whole struct. Over SSH each probe is ~60ms
//! of latency, and there are thirty of them; asking separately turned a page
//! load into two seconds of serial waiting.

use serde::Serialize;

use crate::host::{Host, HostError};

/// One more PCI function of a card — its HDMI audio, its USB-C controller.
/// VFIO claims a group, so these go wherever the display function goes.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Function {
    pub address: String,
    pub ids: String,
    pub driver: String,
}

/// "VGA compatible controller: NVIDIA Corporation GA104GL [RTX A4000]" →
/// "RTX A4000". The bracketed marketing name is the one a person recognises;
/// the rest is the PCI database being thorough.
///
/// A card the database has no marketing name for keeps its device name but
/// loses the vendor's legal one: "ASPEED Technology, Inc. ASPEED Graphics
/// Family" is "ASPEED Graphics Family", which fits a row title; with the
/// vendor it was cut off at "Graphi…" in two panels and a setup step.
///
/// One function, because the setup step's title and the Graphics row name
/// the same card, and two spellings of the same shortening drifted once.
pub fn short_gpu_name(full: &str) -> String {
    // "VGA compatible controller: " is the class, not the card, and lspci's
    // "(rev a1)" at the end is the silicon stepping, which no person uses to
    // tell their cards apart.
    let s = full.split(": ").last().unwrap_or(full).trim();
    let s = match s.rfind(" (rev ") {
        Some(i) if s.ends_with(')') => s[..i].trim(),
        _ => s,
    };
    if let (Some(a), Some(b)) = (s.rfind('['), s.rfind(']')) {
        if a < b && s[b + 1..].trim().is_empty() {
            let inner = s[a + 1..b].trim();
            if !inner.is_empty() {
                return inner.to_string();
            }
        }
    }
    // No marketing name. Drop the vendor's legal name, which the database
    // spells in full, and an alias it puts in brackets ahead of the device
    // ("Advanced Micro Devices, Inc. [AMD/ATI] Navi 31").
    let mut rest = s;
    let cut = [
        "Corporation ",
        "Inc. ",
        "Ltd. ",
        "Limited ",
        "LLC ",
        "GmbH ",
        "Co. ",
    ]
    .iter()
    .filter_map(|e| rest.find(e).map(|i| i + e.len()))
    .min();
    if let Some(i) = cut {
        rest = &rest[i..];
    }
    if rest.starts_with('[') {
        if let Some(b) = rest.find("] ") {
            rest = &rest[b + 2..];
        }
    }
    let rest = rest.trim();
    if rest.is_empty() {
        s.to_string()
    } else {
        rest.to_string()
    }
}

/// A discrete graphics device, and whether it can be handed to one guest.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Gpu {
    /// PCI address in domain form, e.g. `0000:78:00.0`.
    pub address: String,
    /// `vendor:device` as lspci prints it, e.g. `10de:24b0`. This is what
    /// `vfio-pci.ids=` wants.
    pub ids: String,
    /// Human name from the PCI database.
    pub name: String,
    /// Kernel driver currently bound, empty when nothing is.
    pub driver: String,
    /// IOMMU group number, or `None` when the IOMMU is off — in which case
    /// passthrough is not merely unconfigured, it is unavailable.
    pub iommu_group: Option<u32>,
    /// Every other PCI address sharing this device's IOMMU group.
    ///
    /// Load-bearing: VFIO claims a *group*, not a device. A GPU grouped with
    /// the host's SATA controller cannot be passed through without taking the
    /// host's disks with it, and the only honest thing to do is say so before
    /// anybody reboots.
    pub group_siblings: Vec<String>,
    /// True when this is the display the host itself is using. Passing it
    /// through blinds the console, which is survivable headless and is not
    /// survivable on someone's desk.
    pub is_boot_vga: bool,
    /// True when the vendor is AMD or Intel *and* the device is the CPU's
    /// integrated graphics — the common "this is the only GPU" case.
    pub integrated: bool,
    /// The card's other functions in the same IOMMU group. Bound and
    /// attached together with the display: a group with one function left
    /// on its host driver is "not viable" and the guest refuses to start.
    pub functions: Vec<Function>,
}

impl Gpu {
    /// The card as a person says it — see [`short_gpu_name`].
    pub fn short_name(&self) -> String {
        short_gpu_name(&self.name)
    }

    /// Every `vendor:device` the card is made of — what `bind-vfio` takes.
    pub fn all_ids(&self) -> Vec<&str> {
        let mut v = vec![self.ids.as_str()];
        for f in &self.functions {
            if !v.contains(&f.ids.as_str()) {
                v.push(f.ids.as_str());
            }
        }
        v
    }

    /// Every PCI address the card is made of — what the guest is given.
    pub fn all_addresses(&self) -> Vec<&str> {
        std::iter::once(self.address.as_str())
            .chain(self.functions.iter().map(|f| f.address.as_str()))
            .collect()
    }

    /// Every function is vfio-pci's — the group is viable.
    pub fn fully_bound(&self) -> bool {
        self.driver == "vfio-pci" && self.functions.iter().all(|f| f.driver == "vfio-pci")
    }

    /// The functions still on their host driver, as "addr (driver)".
    pub fn held_functions(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.driver != "vfio-pci" {
            out.push(format!(
                "{} ({})",
                self.address,
                if self.driver.is_empty() {
                    "no driver"
                } else {
                    &self.driver
                }
            ));
        }
        for f in &self.functions {
            if f.driver != "vfio-pci" {
                out.push(format!(
                    "{} ({})",
                    f.address,
                    if f.driver.is_empty() {
                        "no driver"
                    } else {
                        &f.driver
                    }
                ));
            }
        }
        out
    }

    /// The PCI device this address belongs to — everything but the function.
    ///
    /// `0000:01:00.1` and `0000:01:00.0` are two *functions* of one device:
    /// the display and its HDMI audio. A discrete card is always several of
    /// these, and newer ones add USB-C and UCSI functions too.
    fn device_of(address: &str) -> &str {
        match address.rfind('.') {
            Some(dot) => &address[..dot],
            None => address,
        }
    }

    /// Group siblings that are a **different physical device**.
    ///
    /// VFIO claiming "the whole group" is exactly what you want when the group
    /// is one card: a guest given a GPU wants its audio function too. Counting
    /// those as foreign blocked passthrough on essentially every discrete GPU
    /// — measured on an RTX A4000 whose only sibling was its own `…:00.1` —
    /// which is the feature this package exists for.
    pub fn foreign_siblings(&self) -> Vec<&str> {
        let own = Self::device_of(&self.address);
        self.group_siblings
            .iter()
            .map(String::as_str)
            .filter(|s| Self::device_of(s) != own)
            .collect()
    }

    /// Whether this card could be given to a guest exclusively.
    ///
    /// Three ways it cannot: the IOMMU is off, the group holds **another
    /// device** the host needs, or it is the host's only display.
    ///
    /// `others` is every display device on the box, so the third can be
    /// decided rather than guessed. It used to be a flat "is it boot_vga",
    /// which on a server with a BMC refused the one card anybody would ever
    /// pass through — the console there is the BMC and survives perfectly. A
    /// single-GPU desktop still gets the refusal, because there the console
    /// really does go dark.
    pub fn passthrough_blockers(&self, others: &[Gpu]) -> Vec<String> {
        let mut out = Vec::new();
        if self.iommu_group.is_none() {
            out.push("the IOMMU is off, so no device can be isolated for a guest".to_string());
        }
        let foreign = self.foreign_siblings();
        if !foreign.is_empty() {
            out.push(format!(
                "its IOMMU group also holds {} — VFIO claims the whole group, so those go to the guest too",
                foreign.join(", ")
            ));
        }
        if self.is_boot_vga && !others.iter().any(|g| g.address != self.address) {
            out.push(
                "it is the host's only display — passing it through leaves the console dark"
                    .to_string(),
            );
        }
        out
    }
}

/// Which package manager the host speaks, and therefore how anything is installed.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PackageManager {
    Pacman,
    Apt,
    Dnf,
    /// Nothing recognised. Provisioning stops at the plan and explains itself
    /// rather than guessing at a command that might do something else.
    Unknown,
}

impl PackageManager {
    /// The install command for a set of packages, already non-interactive.
    pub fn install(&self, packages: &[&str]) -> Option<String> {
        let list = packages.join(" ");
        Some(match self {
            // --needed so a re-run is free rather than a reinstall.
            Self::Pacman => format!("sudo pacman -S --needed --noconfirm {list}"),
            Self::Apt => format!("sudo DEBIAN_FRONTEND=noninteractive apt-get install -y {list}"),
            Self::Dnf => format!("sudo dnf install -y {list}"),
            Self::Unknown => return None,
        })
    }
}

/// How the kernel command line is set, and therefore how the IOMMU gets
/// turned on.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Bootloader {
    /// systemd-boot: one `options` line per entry under /boot/loader/entries.
    SystemdBoot,
    /// GRUB: `GRUB_CMDLINE_LINUX_DEFAULT` in /etc/default/grub, then regenerate.
    Grub,
    /// Something else, or a unified kernel image whose cmdline is baked in.
    /// Reported rather than guessed at — editing the wrong bootloader's config
    /// is a silent no-op that only shows up as "passthrough still doesn't work".
    Unknown,
}

/// Which CPU, and therefore which IOMMU parameter.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CpuVendor {
    Amd,
    Intel,
    Unknown,
}

impl CpuVendor {
    /// The kernel parameters that enable the IOMMU on this CPU.
    ///
    /// `iommu=pt` (pass-through mode) is not optional in practice: without it
    /// the DMA remapping overhead shows up as a measurable hit on guest I/O,
    /// and every passthrough guide sets it. Included here so it cannot be
    /// forgotten at the one site that assembles the command.
    pub fn iommu_params(&self) -> &'static [&'static str] {
        match self {
            Self::Amd => &["amd_iommu=on", "iommu=pt"],
            Self::Intel => &["intel_iommu=on", "iommu=pt"],
            Self::Unknown => &["iommu=pt"],
        }
    }
}

/// How this process gets to do privileged things on the host.
///
/// Hypervisor setup is root work — installing packages, enabling units,
/// writing `/etc/modprobe.d`, editing the kernel command line. On a Leo hub
/// the service account deliberately has no blanket sudo, so the package ships
/// a root-owned wrapper with a **closed verb set** and one sudoers rule for
/// it. Without that rule the package is still useful: it reports, plans and
/// hands over the exact commands. It simply does not pretend it can apply them.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Privilege {
    /// We are root already — an SSH'd compute box where root is the login.
    Root,
    /// `sudo -n` works for anything. Common on a dedicated compute machine.
    PasswordlessSudo,
    /// The narrow wrapper is installed and sudo-able. The intended path on a hub.
    Wrapper,
    /// Nothing. Read-only: inventory, plan, and copyable commands.
    None,
}

impl Privilege {
    /// How the host is acted on, for the Host panel.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Root => "root",
            Self::PasswordlessSudo => "passwordless sudo",
            Self::Wrapper => "leo-spaces-admin (one sudoers rule)",
            Self::None => "none — setup steps are copy-only",
        }
    }

    /// Wrap a *wrapper verb* so it actually runs with privilege.
    ///
    /// Only the wrapper's own closed verbs come through here. Arbitrary shell
    /// never does — that is the entire point of the verb set, and the reason
    /// the sudoers rule can name one program and still be narrow.
    pub fn wrap(&self, verb: &str) -> Option<String> {
        Some(match self {
            Self::Root => format!("{WRAPPER} {verb}"),
            Self::PasswordlessSudo => format!("sudo -n {WRAPPER} {verb}"),
            Self::Wrapper => format!("sudo -n {WRAPPER} {verb}"),
            Self::None => return None,
        })
    }

    pub fn can_apply(&self) -> bool {
        !matches!(self, Self::None)
    }
}

/// Where the root-owned wrapper lives once an admin has installed it.
pub const WRAPPER: &str = "/usr/local/bin/leo-spaces-admin";

/// One file in `/var/lib/leo-spaces/iso`, in whichever of its states.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct IsoFile {
    /// The name without `.part`, so a download in flight and the image it
    /// becomes are the same entry.
    pub file: String,
    pub size_mb: u64,
    pub state: IsoState,
    /// The helper's last word, when a download failed.
    pub failure: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IsoState {
    Ready,
    Downloading,
    Failed,
}

/// The photograph.
#[derive(Debug, Clone, Serialize)]
pub struct Facts {
    pub host: String,
    pub reachable: bool,
    /// Why not, when `reachable` is false. The page shows this instead of a
    /// screen full of zeroes that look like a host with no CPU.
    pub unreachable_reason: Option<String>,

    pub hostname: String,
    pub os: String,
    pub os_id: String,
    pub kernel: String,
    pub package_manager: PackageManager,
    pub bootloader: Bootloader,

    pub cpu_model: String,
    pub cpu_vendor: CpuVendor,
    pub cpu_threads: u32,
    pub memory_mb: u64,
    pub disk_free_gb: u64,

    /// Hardware virtualisation in the CPU flags. False on a box that cannot
    /// host a guest at any speed worth having, and the one fact that makes
    /// the whole package inapplicable.
    pub virt_supported: bool,
    /// /dev/kvm exists and is usable.
    pub kvm_available: bool,
    /// Number of IOMMU groups the kernel exposed. Zero means the IOMMU is off.
    pub iommu_groups: u32,
    pub kernel_cmdline: String,

    pub qemu_installed: bool,
    pub libvirt_installed: bool,
    pub libvirt_running: bool,
    pub ovmf_installed: bool,
    pub swtpm_installed: bool,
    pub virtiofsd_installed: bool,
    /// The `default` libvirt network is defined and active — what a guest gets
    /// NAT and DHCP from before anyone configures a bridge.
    pub default_network_active: bool,
    /// The package's own storage pool is defined and running.
    pub pool_ready: bool,
    /// The host firewall has been opened for guests: the snippet is written
    /// and its unit is in force. (Leo OS drops a guest's DHCP and forwarding
    /// by default.)
    pub guest_net_open: bool,
    /// The virtio-win driver ISO is in the package's media directory.
    pub virtio_iso_present: bool,
    /// Every installer image in the media directory, finished or not.
    pub isos: Vec<IsoFile>,
    /// `(os id, MB)` for each image waiting in the import directory.
    pub staged: Vec<(String, u64)>,
    /// The `vendor:device` ids the box's modprobe config hands to vfio-pci at
    /// boot — what `bind-vfio` wrote, whether or not a reboot has happened.
    pub vfio_ids: Vec<String>,
    /// That config predates the current boot: the host has rebooted since it
    /// was written, so a card still held by its own driver is a binding that
    /// did not take, not one waiting for a reboot.
    pub vfio_config_predates_boot: bool,
    /// The user libvirt commands will run as is in the `libvirt` group.
    pub in_libvirt_group: bool,
    pub guacd_reachable: bool,

    /// How much this process may change about the host.
    pub privilege: Privilege,
    /// The wrapper binary exists on disk.
    pub wrapper_installed: bool,
    /// The wrapper exists *and* the sudoers rule for it is live. Both halves
    /// matter: a wrapper with no rule is a file nothing can run, which is the
    /// state a half-finished enable leaves behind.
    pub wrapper_usable: bool,

    pub gpus: Vec<Gpu>,
}

impl Facts {
    pub fn iommu_enabled(&self) -> bool {
        self.iommu_groups > 0
    }

    /// Whether this host can run guests *at all*, right now.
    pub fn ready_for_guests(&self) -> bool {
        self.kvm_available && self.libvirt_running && self.qemu_installed && self.ovmf_installed
    }

    /// A one-line summary for the sidebar hero.
    pub fn status_label(&self) -> &'static str {
        if !self.reachable {
            "Unreachable"
        } else if !self.virt_supported {
            "No virtualisation"
        } else if self.ready_for_guests() {
            "Ready"
        } else if self.libvirt_installed {
            "Needs setup"
        } else {
            "Not installed"
        }
    }

    pub fn status_color(&self) -> &'static str {
        match self.status_label() {
            "Ready" => "#30d158",
            "Needs setup" => "#ff9f0a",
            "Unreachable" | "No virtualisation" => "#ff453a",
            _ => "#98989d",
        }
    }
}

/// One shell payload that prints every fact as `key<TAB>value`.
///
/// Emitting TSV and parsing it in Rust rather than building JSON in bash: the
/// quoting rules for a PCI device name containing `[AMD/ATI]` are a trap, and
/// a single unescaped bracket would turn the whole probe into a parse error
/// that reads as an unreachable host.
const PROBE: &str = r#"
set -u
p() { printf '%s\t%s\n' "$1" "$2"; }

p hostname "$(uname -n 2>/dev/null || echo unknown)"
p kernel "$(uname -r 2>/dev/null || echo unknown)"
if [ -r /etc/os-release ]; then
  . /etc/os-release
  p os "${PRETTY_NAME:-${NAME:-unknown}}"
  p os_id "${ID:-unknown}"
fi

if command -v pacman >/dev/null 2>&1; then p pkgmgr pacman
elif command -v apt-get >/dev/null 2>&1; then p pkgmgr apt
elif command -v dnf >/dev/null 2>&1; then p pkgmgr dnf
else p pkgmgr unknown; fi

# A UKI or an entry directory both mean systemd-boot owns the cmdline; GRUB is
# only GRUB when its config file is actually there to edit.
if [ -d /boot/loader/entries ] && command -v bootctl >/dev/null 2>&1; then p bootloader systemd_boot
elif [ -f /etc/default/grub ]; then p bootloader grub
else p bootloader unknown; fi

p cpu_model "$(awk -F': ' '/^model name/{print $2; exit}' /proc/cpuinfo 2>/dev/null)"
if grep -qm1 '^vendor_id.*AuthenticAMD' /proc/cpuinfo 2>/dev/null; then p cpu_vendor amd
elif grep -qm1 '^vendor_id.*GenuineIntel' /proc/cpuinfo 2>/dev/null; then p cpu_vendor intel
else p cpu_vendor unknown; fi
p cpu_threads "$(grep -c '^processor' /proc/cpuinfo 2>/dev/null || echo 0)"
p memory_mb "$(awk '/^MemTotal:/{printf "%d", $2/1024}' /proc/meminfo 2>/dev/null || echo 0)"
p disk_free_gb "$(df -BG --output=avail / 2>/dev/null | tail -1 | tr -dc '0-9' || echo 0)"

# -qE, not -qmE1: bundling `m` with a following `E` makes grep read "E1" as
# the -m argument and exit 2 (invalid usage), which the `||` then reports as
# "no virtualisation support" on every host alive. See the regression test.
grep -qE '^flags.*(svm|vmx)' /proc/cpuinfo 2>/dev/null && p virt_supported 1 || p virt_supported 0
[ -e /dev/kvm ] && p kvm 1 || p kvm 0
p iommu_groups "$(ls /sys/kernel/iommu_groups 2>/dev/null | wc -l)"
p cmdline "$(cat /proc/cmdline 2>/dev/null | tr -d '\n')"

command -v qemu-system-x86_64 >/dev/null 2>&1 && p qemu 1 || p qemu 0
command -v virsh >/dev/null 2>&1 && p libvirt 1 || p libvirt 0
command -v swtpm >/dev/null 2>&1 && p swtpm 1 || p swtpm 0
command -v virtiofsd >/dev/null 2>&1 && p virtiofsd 1 || p virtiofsd 0

# Any of the modular or monolithic daemons being up counts as running: Arch
# ships the monolithic libvirtd, Debian has moved to virtqemud, and a package
# that only knew one of them would report a working host as broken.
if systemctl is-active --quiet libvirtd 2>/dev/null \
  || systemctl is-active --quiet libvirtd.socket 2>/dev/null \
  || systemctl is-active --quiet virtqemud.socket 2>/dev/null; then p libvirt_running 1
else p libvirt_running 0; fi

# OVMF lives in a different place on every distro, and Windows will not boot
# without it. Checked by looking rather than by trusting the package name.
#
# ONE LOCATION AT A TIME, and that is the whole point. `ls a b c` exits
# NON-ZERO when any one of its arguments is missing, even if the others are
# there — and a box only ever has one distribution's layout, so the combined
# form answered "absent" on every machine, however much OVMF was installed.
# The packages step therefore stayed Pending forever after a successful
# install, which nobody saw until the privileged half started working: before
# that the step was Blocked and never ran at all.
if ls /usr/share/edk2/x64/OVMF_CODE*.fd >/dev/null 2>&1 \
  || ls /usr/share/edk2-ovmf/x64/OVMF_CODE*.fd >/dev/null 2>&1 \
  || ls /usr/share/OVMF/OVMF_CODE*.fd >/dev/null 2>&1 \
  || ls /usr/share/qemu/ovmf-x86_64-code.bin >/dev/null 2>&1; then
  p ovmf 1
else p ovmf 0; fi

if virsh --connect qemu:///system net-info default 2>/dev/null | grep -qi '^Active:.*yes'; then
  p default_net 1
else p default_net 0; fi

# The host firewall opened for guests — see the helper's open-guest-network.
# The ruleset itself needs root to read; the file plus a live unit is what
# can be seen from here, and the unit re-applies the file on every boot.
if [ -s /etc/nftables.d/leo-spaces-guests.conf ] && systemctl is-active leo-spaces-guest-net.service >/dev/null 2>&1; then
  p guest_net 1
else p guest_net 0; fi

# The storage pool, probed rather than assumed. The step that creates it used
# to report `satisfied: false` unconditionally — with a comment saying that
# reporting it pending on a host where it already exists "would be a lie", and
# then telling exactly that lie. The cost was not cosmetic: `virtio` requires
# `storage`, so a permanently-unsatisfied storage step left virtio permanently
# unapplicable, and the setup list could never reach a finished state.
if virsh --connect qemu:///system pool-info leo-spaces 2>/dev/null | grep -qi '^State:.*running'; then
  p pool 1
else p pool 0; fi

# The driver ISO, for the same reason as the pool: the step that fetches it
# reported pending forever, so a finished host showed an unfinished list.
if [ -s /var/lib/leo-spaces/iso/virtio-win.iso ]; then p virtio_iso 1; else p virtio_iso 0; fi

# Images fetched as leo and waiting for root to adopt them — keyed by OS id,
# which is the file's stem in the import directory.
for f in /var/lib/leo/spaces-import/*.iso; do
  [ -e "$f" ] || continue
  p staged "$(basename "$f" .iso)|$(( $(stat -c %s "$f" 2>/dev/null || echo 0) / 1048576 ))"
done

# Every installer image, finished or in flight. A `.part` is a download the
# helper is still writing; a `.log` with neither is one that failed, and its
# last line is why. The driver ISO is a step, not an installer, so it is
# skipped here.
for f in /var/lib/leo-spaces/iso/*.iso /var/lib/leo-spaces/iso/*.iso.part /var/lib/leo-spaces/iso/*.iso.log; do
  [ -e "$f" ] || continue
  name=$(basename "$f")
  case "$name" in virtio-win.iso*) continue ;; esac
  case "$name" in
    *.iso)      p iso "$name|$(( $(stat -c %s "$f" 2>/dev/null || echo 0) / 1048576 ))|ready|" ;;
    *.iso.part) p iso "${name%.part}|$(( $(stat -c %s "$f" 2>/dev/null || echo 0) / 1048576 ))|downloading|" ;;
    *.iso.log)
      base="${name%.log}"
      if [ ! -e "/var/lib/leo-spaces/iso/$base" ] && [ ! -e "/var/lib/leo-spaces/iso/$base.part" ]; then
        p iso "$base|0|failed|$(tail -n 1 "$f" 2>/dev/null | tr -d '|' | cut -c1-200)"
      fi ;;
  esac
done

# What bind-vfio has configured, and whether a reboot has happened since — the
# difference between "reboot to finish" and "the binding did not take".
vfio_ids=$(sed -n 's/^options vfio-pci ids=\([^ ]*\).*/\1/p' /etc/modprobe.d/leo-spaces-vfio.conf 2>/dev/null | head -1)
p vfio_ids "${vfio_ids:-none}"
vfio_stale=0
if [ -n "$vfio_ids" ]; then
  btime=$(awk '/^btime/ {print $2}' /proc/stat 2>/dev/null)
  mtime=$(stat -c %Y /etc/modprobe.d/leo-spaces-vfio.conf 2>/dev/null)
  if [ -n "$btime" ] && [ -n "$mtime" ] && [ "$mtime" -lt "$btime" ]; then vfio_stale=1; fi
fi
p vfio_predates_boot $vfio_stale

id -nG 2>/dev/null | tr ' ' '\n' | grep -qx libvirt && p libvirt_group 1 || p libvirt_group 0

# guacd is how a browser gets pixels out of a guest. Optional, so its absence
# is a fact rather than a failure.
(exec 3<>/dev/tcp/127.0.0.1/4822) >/dev/null 2>&1 && p guacd 1 || p guacd 0

# Privilege, most-direct first. Each probe is non-interactive: `sudo -n` fails
# rather than prompting, so a box with a password-gated sudo reports None
# instead of hanging a page load on a password nobody is there to type.
[ "$(id -u)" = "0" ] && p root 1 || p root 0
sudo -n true >/dev/null 2>&1 && p sudo 1 || p sudo 0
[ -x /usr/local/bin/leo-spaces-admin ] && p wrapper 1 || p wrapper 0
sudo -n /usr/local/bin/leo-spaces-admin version >/dev/null 2>&1 && p wrapper_ok 1 || p wrapper_ok 0

# One line per display device. Walking sysfs rather than parsing lspci output
# because the IOMMU group and the bound driver are both only there.
#
# Shell builtins throughout — `read` from the sysfs file, `${x##*/}` for the
# last path component, `${x#0x}` for the prefix — rather than `cat`, `basename`
# and `sed` in subshells. Each of those is a fork and an exec, about a
# millisecond apiece on the measured host, and the old loop spent thirty of
# them per card. The probe as a whole is some 150 small commands and runs on
# every page load, so forks are its cost; the output is byte-for-byte the same.
for dev in /sys/bus/pci/devices/*; do
  [ -r "$dev/class" ] || continue
  class=""; read -r class < "$dev/class" 2>/dev/null || true
  case "$class" in
    0x0300*|0x0302*) ;;
    *) continue ;;
  esac
  addr="${dev##*/}"
  vendor=""; read -r vendor < "$dev/vendor" 2>/dev/null || true; vendor="${vendor#0x}"
  device=""; read -r device < "$dev/device" 2>/dev/null || true; device="${device#0x}"
  driver=""
  if [ -L "$dev/driver" ]; then driver=$(readlink -f "$dev/driver"); driver="${driver##*/}"; fi
  group=""
  if [ -L "$dev/iommu_group" ]; then group=$(readlink -f "$dev/iommu_group"); group="${group##*/}"; fi
  siblings=""
  if [ -n "$group" ] && [ -d "/sys/kernel/iommu_groups/$group/devices" ]; then
    for s in /sys/kernel/iommu_groups/"$group"/devices/*; do
      sb="${s##*/}"
      [ "$sb" = "$addr" ] && continue
      siblings="${siblings:+$siblings,}$sb"
    done
  fi
  bootvga=0
  if [ -r "$dev/boot_vga" ]; then bv=""; read -r bv < "$dev/boot_vga" 2>/dev/null || true; [ "$bv" = "1" ] && bootvga=1; fi
  # The card's other functions: same bus and slot, in the same group. Each
  # as addr=vendor:device=driver, so the plan can bind and attach them all.
  functions=""
  slot="${addr%.*}"
  for s in /sys/bus/pci/devices/"$slot".*; do
    sb="${s##*/}"
    [ "$sb" = "$addr" ] && continue
    if [ -L "$s/iommu_group" ]; then sg=$(readlink -f "$s/iommu_group"); [ "${sg##*/}" != "$group" ] && continue; fi
    fd=""; if [ -L "$s/driver" ]; then fd=$(readlink -f "$s/driver"); fd="${fd##*/}"; fi
    fv=""; read -r fv < "$s/vendor" 2>/dev/null || true
    fdv=""; read -r fdv < "$s/device" 2>/dev/null || true
    functions="${functions:+$functions,}$sb=${fv#0x}:${fdv#0x}=$fd"
  done
  name=$(lspci -s "$addr" 2>/dev/null | cut -d' ' -f2- | sed 's/^[^:]*: //')
  printf 'gpu\t%s|%s:%s|%s|%s|%s|%s|%s|%s\n' \
    "$addr" "$vendor" "$device" "$driver" "$group" "$siblings" "$bootvga" "$functions" "$name"
done
"#;

/// Take the photograph.
pub async fn probe(host: &Host) -> Result<Facts, HostError> {
    let mut f = probe_blank(host);

    // A read, so it gets the read timeout — see `host::READ_TIMEOUT`.
    let out = match host
        .run_with_timeout(PROBE, crate::host::READ_TIMEOUT)
        .await
    {
        Ok(o) => o,
        Err(e @ HostError::Timeout { .. }) => {
            f.unreachable_reason = Some(e.to_string());
            return Ok(f);
        }
        Err(e) => {
            f.unreachable_reason = Some(e.to_string());
            return Ok(f);
        }
    };

    // The probe is defensive throughout, so a non-zero status means the shell
    // itself could not run — an unreachable host, not a host missing a tool.
    if !out.ok() && out.stdout.trim().is_empty() {
        f.unreachable_reason = Some(out.failure());
        return Ok(f);
    }

    f.reachable = true;
    f.unreachable_reason = None;

    let mut is_root = false;
    let mut has_sudo = false;

    for line in out.stdout.lines() {
        let Some((key, value)) = line.split_once('\t') else {
            continue;
        };
        let v = value.trim();
        match key {
            "hostname" => f.hostname = v.to_string(),
            "kernel" => f.kernel = v.to_string(),
            "os" => f.os = v.to_string(),
            "os_id" => f.os_id = v.to_string(),
            "pkgmgr" => {
                f.package_manager = match v {
                    "pacman" => PackageManager::Pacman,
                    "apt" => PackageManager::Apt,
                    "dnf" => PackageManager::Dnf,
                    _ => PackageManager::Unknown,
                }
            }
            "bootloader" => {
                f.bootloader = match v {
                    "systemd_boot" => Bootloader::SystemdBoot,
                    "grub" => Bootloader::Grub,
                    _ => Bootloader::Unknown,
                }
            }
            "cpu_model" => f.cpu_model = v.to_string(),
            "cpu_vendor" => {
                f.cpu_vendor = match v {
                    "amd" => CpuVendor::Amd,
                    "intel" => CpuVendor::Intel,
                    _ => CpuVendor::Unknown,
                }
            }
            "cpu_threads" => f.cpu_threads = v.parse().unwrap_or(0),
            "memory_mb" => f.memory_mb = v.parse().unwrap_or(0),
            "disk_free_gb" => f.disk_free_gb = v.parse().unwrap_or(0),
            "virt_supported" => f.virt_supported = v == "1",
            "kvm" => f.kvm_available = v == "1",
            "iommu_groups" => f.iommu_groups = v.parse().unwrap_or(0),
            "cmdline" => f.kernel_cmdline = v.to_string(),
            "qemu" => f.qemu_installed = v == "1",
            "libvirt" => f.libvirt_installed = v == "1",
            "libvirt_running" => f.libvirt_running = v == "1",
            "ovmf" => f.ovmf_installed = v == "1",
            "swtpm" => f.swtpm_installed = v == "1",
            "virtiofsd" => f.virtiofsd_installed = v == "1",
            "default_net" => f.default_network_active = v == "1",
            "guest_net" => f.guest_net_open = v == "1",
            "pool" => f.pool_ready = v == "1",
            "virtio_iso" => f.virtio_iso_present = v == "1",
            "iso" => {
                if let Some(i) = parse_iso(v) {
                    f.isos.push(i);
                }
            }
            "staged" => {
                let mut it = v.splitn(2, '|');
                let id = it.next().unwrap_or("").trim();
                if !id.is_empty() {
                    f.staged.push((
                        id.to_string(),
                        it.next().and_then(|m| m.trim().parse().ok()).unwrap_or(0),
                    ));
                }
            }
            "vfio_ids" => {
                f.vfio_ids = v
                    .split(',')
                    .filter(|s| !s.is_empty() && *s != "none")
                    .map(str::to_string)
                    .collect()
            }
            "vfio_predates_boot" => f.vfio_config_predates_boot = v == "1",
            "libvirt_group" => f.in_libvirt_group = v == "1",
            "guacd" => f.guacd_reachable = v == "1",
            "root" => is_root = v == "1",
            "sudo" => has_sudo = v == "1",
            "wrapper" => f.wrapper_installed = v == "1",
            "wrapper_ok" => f.wrapper_usable = v == "1",
            "gpu" => {
                if let Some(g) = parse_gpu(v) {
                    f.gpus.push(g);
                }
            }
            _ => {}
        }
    }

    // Most direct wins. The wrapper is listed last not because it is worst but
    // because it is narrowest: if this process is already root, or holds
    // blanket sudo, routing through a verb set would only limit what the
    // operator can be helped with.
    f.privilege = if is_root {
        Privilege::Root
    } else if has_sudo {
        Privilege::PasswordlessSudo
    } else if f.wrapper_usable {
        Privilege::Wrapper
    } else {
        Privilege::None
    };

    Ok(f)
}

/// `file|size_mb|state|failure`
fn parse_iso(raw: &str) -> Option<IsoFile> {
    let parts: Vec<&str> = raw.splitn(4, '|').collect();
    if parts.len() < 3 || parts[0].trim().is_empty() {
        return None;
    }
    let state = match parts[2].trim() {
        "ready" => IsoState::Ready,
        "downloading" => IsoState::Downloading,
        "failed" => IsoState::Failed,
        _ => return None,
    };
    let failure = parts
        .get(3)
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    Some(IsoFile {
        file: parts[0].trim().to_string(),
        size_mb: parts[1].trim().parse().unwrap_or(0),
        state,
        failure,
    })
}

impl Facts {
    /// What the host has of one installer file, if anything.
    pub fn iso(&self, file: &str) -> Option<&IsoFile> {
        self.isos.iter().find(|i| i.file == file)
    }

    /// The image is on the host, whole.
    pub fn has_iso(&self, file: &str) -> bool {
        self.iso(file).is_some_and(|i| i.state == IsoState::Ready)
    }
}

/// `addr|vendor:device|driver|group|siblings|bootvga|functions|name`
///
/// Fields are positional and pipe-separated, and the name comes last because
/// it is the only one that can contain anything. `splitn` with the field count
/// means a name with a pipe in it stays whole instead of shifting every field
/// after it.
fn parse_gpu(raw: &str) -> Option<Gpu> {
    let parts: Vec<&str> = raw.splitn(8, '|').collect();
    if parts.len() < 8 {
        return None;
    }
    let address = parts[0].trim();
    if address.is_empty() {
        return None;
    }
    let ids = parts[1].trim().to_string();
    let driver = parts[2].trim().to_string();
    let iommu_group = parts[3].trim().parse::<u32>().ok();
    let group_siblings = parts[4]
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    let is_boot_vga = parts[5].trim() == "1";
    let functions = parts[6]
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .filter_map(|s| {
            let mut it = s.splitn(3, '=');
            Some(Function {
                address: it.next()?.to_string(),
                ids: it.next()?.to_string(),
                driver: it.next().unwrap_or("").to_string(),
            })
        })
        .collect();
    let name = parts[7].trim().to_string();

    // Integrated graphics are identified by name rather than by vendor: AMD
    // and Intel both ship discrete cards too, and calling every AMD GPU
    // integrated would hide a real passthrough candidate.
    let lower = name.to_ascii_lowercase();
    let integrated = lower.contains("integrated")
        || lower.contains("raphael")
        || lower.contains("granite ridge")
        || lower.contains("phoenix")
        || lower.contains("renoir")
        || lower.contains("cezanne")
        || lower.contains("uhd graphics")
        || lower.contains("iris");

    Some(Gpu {
        address: address.to_string(),
        ids,
        name,
        driver,
        iommu_group,
        group_siblings,
        is_boot_vga,
        integrated,
        functions,
    })
}

/// A `Facts` with nothing known yet.
///
/// Public because the provisioner's tests build hosts by overriding fields on
/// top of it — a struct with thirty fields is unreadable constructed in full,
/// and a test that names only what it is varying is a test that says what it
/// is about.
pub fn probe_blank(host: &Host) -> Facts {
    Facts {
        host: host.to_string(),
        reachable: false,
        unreachable_reason: Some("not probed".into()),
        hostname: String::new(),
        os: String::new(),
        os_id: String::new(),
        kernel: String::new(),
        package_manager: PackageManager::Unknown,
        bootloader: Bootloader::Unknown,
        cpu_model: String::new(),
        cpu_vendor: CpuVendor::Unknown,
        cpu_threads: 0,
        memory_mb: 0,
        disk_free_gb: 0,
        virt_supported: false,
        kvm_available: false,
        iommu_groups: 0,
        kernel_cmdline: String::new(),
        qemu_installed: false,
        libvirt_installed: false,
        libvirt_running: false,
        ovmf_installed: false,
        swtpm_installed: false,
        virtiofsd_installed: false,
        default_network_active: false,
        guest_net_open: false,
        pool_ready: false,
        virtio_iso_present: false,
        isos: Vec::new(),
        staged: Vec::new(),
        vfio_ids: Vec::new(),
        vfio_config_predates_boot: false,
        in_libvirt_group: false,
        guacd_reachable: false,
        privilege: Privilege::None,
        wrapper_installed: false,
        wrapper_usable: false,
        gpus: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_gpu_name_shortens_to_the_part_a_person_recognises() {
        assert_eq!(
            short_gpu_name("VGA compatible controller: NVIDIA Corporation GA104GL [RTX A4000]"),
            "RTX A4000"
        );
        assert_eq!(
            short_gpu_name(
                "Advanced Micro Devices, Inc. [AMD/ATI] Granite Ridge [Radeon Graphics]"
            ),
            "Radeon Graphics"
        );
        // The revision lspci appends is a stepping, not a name — and it
        // sits after the bracket, so it must not hide the marketing name.
        assert_eq!(
            short_gpu_name(
                "VGA compatible controller: NVIDIA Corporation GA104GL [RTX A4000] (rev a1)"
            ),
            "RTX A4000"
        );
        assert_eq!(
            short_gpu_name("ASPEED Technology, Inc. ASPEED Graphics Family (rev 52)"),
            "ASPEED Graphics Family"
        );
        // No brackets: the vendor's legal name goes, the device's stays.
        assert_eq!(
            short_gpu_name(
                "VGA compatible controller: ASPEED Technology, Inc. ASPEED Graphics Family"
            ),
            "ASPEED Graphics Family"
        );
        assert_eq!(
            short_gpu_name("Matrox Electronics Systems Ltd. MGA G200eW WPCM450"),
            "MGA G200eW WPCM450"
        );
        // An alias in brackets ahead of the device is the vendor again.
        assert_eq!(
            short_gpu_name("Advanced Micro Devices, Inc. [AMD/ATI] Navi 31"),
            "Navi 31"
        );
        // Nothing to strip: fall back to the part after the class.
        assert_eq!(
            short_gpu_name("3D controller: Some Card 9000"),
            "Some Card 9000"
        );
        // A name that is only a vendor is better than an empty title.
        assert_eq!(short_gpu_name("Some Vendor Inc. "), "Some Vendor Inc.");
    }

    use super::*;

    /// A device name with brackets and slashes — `[AMD/ATI] Granite Ridge` is
    /// the real string from the work hub — must survive parsing whole. This is
    /// the case that made TSV-plus-positional-fields the format instead of
    /// JSON assembled in bash.
    #[test]
    fn a_gpu_line_parses_with_punctuation_in_the_name() {
        let g = parse_gpu(
            "0000:78:00.0|1002:13c0|amdgpu|12||1||VGA compatible controller: Advanced Micro Devices, Inc. [AMD/ATI] Granite Ridge [Radeon Graphics]",
        )
        .expect("should parse");
        assert_eq!(g.address, "0000:78:00.0");
        assert_eq!(g.ids, "1002:13c0");
        assert_eq!(g.driver, "amdgpu");
        assert_eq!(g.iommu_group, Some(12));
        assert!(g.group_siblings.is_empty());
        assert!(g.is_boot_vga);
        assert!(g.name.contains("[AMD/ATI]"));
        assert!(g.name.contains("Granite Ridge"));
        // Granite Ridge is the 9950X's integrated display. Calling it discrete
        // would offer the owner a passthrough that blinds their own console.
        assert!(g.integrated);
    }

    #[test]
    fn a_discrete_card_is_not_called_integrated() {
        let g = parse_gpu(
            "0000:01:00.0|10de:24b0|nvidia|15|0000:01:00.1|0|0000:01:00.1=10de:228b=snd_hda_intel|VGA compatible controller: NVIDIA Corporation GA104GL [RTX A4000]",
        )
        .unwrap();
        assert!(!g.integrated);
        assert!(!g.is_boot_vga);
        assert_eq!(g.group_siblings, vec!["0000:01:00.1"]);
        // The card's audio function rides along, with its own driver.
        assert_eq!(g.functions.len(), 1);
        assert_eq!(g.functions[0].ids, "10de:228b");
        assert_eq!(g.functions[0].driver, "snd_hda_intel");
        assert_eq!(g.all_ids(), vec!["10de:24b0", "10de:228b"]);
        assert!(!g.fully_bound());
    }

    /// The blockers are the whole safety story for passthrough. Each one is a
    /// reboot that does not come back the way the owner expected, so each is
    /// reported in words before anything is written.
    #[test]
    fn passthrough_blockers_name_every_reason() {
        // Clean discrete card alone in its group: nothing in the way.
        let clean = Gpu {
            address: "0000:01:00.0".into(),
            ids: "10de:24b0".into(),
            name: "RTX A4000".into(),
            driver: "nvidia".into(),
            iommu_group: Some(15),
            group_siblings: vec![],
            is_boot_vga: false,
            integrated: false,
            functions: vec![],
        };
        assert!(clean
            .passthrough_blockers(std::slice::from_ref(&clean))
            .is_empty());

        // IOMMU off.
        let no_iommu = Gpu {
            iommu_group: None,
            ..clean.clone()
        };
        let b = no_iommu.passthrough_blockers(std::slice::from_ref(&no_iommu));
        assert_eq!(b.len(), 1);
        assert!(b[0].contains("IOMMU is off"));

        // Group shared with something the host needs.
        let shared = Gpu {
            group_siblings: vec!["0000:00:1f.2".into()],
            ..clean.clone()
        };
        let b = shared.passthrough_blockers(std::slice::from_ref(&shared));
        assert!(
            b[0].contains("0000:00:1f.2"),
            "must name the sibling: {b:?}"
        );

        // The host's ONLY display.
        let boot = Gpu {
            is_boot_vga: true,
            ..clean.clone()
        };
        assert!(
            boot.passthrough_blockers(std::slice::from_ref(&boot))[0].contains("console"),
            "the only display must still be refused"
        );
    }

    /// **A card's own audio function is not a foreign device.**
    ///
    /// `01:00.0` and `01:00.1` are two functions of one card — the display and
    /// its HDMI audio — and VFIO claiming both is exactly what a guest wants.
    /// Treating the sibling as foreign blocked passthrough on essentially
    /// every discrete GPU, which is the feature this package is for. Measured
    /// on an RTX A4000 whose only group member besides itself was `…:00.1`.
    #[test]
    fn a_cards_own_functions_are_not_a_blocker() {
        let card = Gpu {
            address: "0000:01:00.0".into(),
            ids: "10de:24b0".into(),
            name: "RTX A4000".into(),
            driver: "nvidia".into(),
            iommu_group: Some(29),
            // Its own audio function, and a USB-C controller as newer cards have.
            group_siblings: vec!["0000:01:00.1".into(), "0000:01:00.2".into()],
            is_boot_vga: false,
            integrated: false,
            functions: vec![],
        };
        assert!(
            card.foreign_siblings().is_empty(),
            "same device, different functions: {:?}",
            card.foreign_siblings()
        );
        assert!(
            card.passthrough_blockers(std::slice::from_ref(&card))
                .is_empty(),
            "a whole card in its own group is passable"
        );

        // A genuinely different device in the group still blocks, and is named.
        let with_foreign = Gpu {
            group_siblings: vec!["0000:01:00.1".into(), "0000:e2:00.0".into()],
            ..card.clone()
        };
        assert_eq!(with_foreign.foreign_siblings(), vec!["0000:e2:00.0"]);
        let b = with_foreign.passthrough_blockers(std::slice::from_ref(&with_foreign));
        assert!(b[0].contains("0000:e2:00.0"), "{b:?}");
        assert!(
            !b[0].contains("0000:01:00.1"),
            "the card's own function must not be listed as in the way: {b:?}"
        );
    }

    /// The console-goes-dark refusal is about the host having NO display left,
    /// not about this card being the one it boots from.
    ///
    /// A server with a BMC is the case that matters: the only card worth
    /// passing through is the boot VGA, and the console is the BMC, which
    /// survives. The flat `is_boot_vga` test refused exactly that machine.
    #[test]
    fn the_boot_card_is_passable_when_the_host_keeps_another_display() {
        let a4000 = Gpu {
            address: "0000:01:00.0".into(),
            ids: "10de:24b0".into(),
            name: "RTX A4000".into(),
            driver: "nvidia".into(),
            iommu_group: Some(29),
            group_siblings: vec!["0000:01:00.1".into()],
            is_boot_vga: true,
            integrated: false,
            functions: vec![],
        };
        let bmc = Gpu {
            address: "0000:e3:00.0".into(),
            ids: "1a03:2000".into(),
            name: "ASPEED Graphics Family".into(),
            driver: "ast".into(),
            iommu_group: Some(10),
            group_siblings: vec!["0000:e2:00.0".into()],
            is_boot_vga: false,
            integrated: false,
            functions: vec![],
        };

        // Alone, it is the only display and stays refused.
        assert!(!a4000
            .passthrough_blockers(std::slice::from_ref(&a4000))
            .is_empty());

        // With the BMC present, the console survives and the card is passable.
        let both = vec![a4000.clone(), bmc.clone()];
        assert!(
            a4000.passthrough_blockers(&both).is_empty(),
            "a boot card is passable when another display remains: {:?}",
            a4000.passthrough_blockers(&both)
        );

        // And the BMC itself is still refused — a different device shares its group.
        let b = bmc.passthrough_blockers(&both);
        assert!(b[0].contains("0000:e2:00.0"), "{b:?}");
    }

    /// The IOMMU parameter differs by vendor, and setting the wrong one is a
    /// reboot that changes nothing — the failure mode being guarded here.
    #[test]
    fn iommu_params_match_the_cpu() {
        assert_eq!(CpuVendor::Amd.iommu_params(), &["amd_iommu=on", "iommu=pt"]);
        assert_eq!(
            CpuVendor::Intel.iommu_params(),
            &["intel_iommu=on", "iommu=pt"]
        );
        // An unknown vendor still gets pass-through mode, which is harmless
        // and correct on both, rather than a guessed vendor parameter.
        assert_eq!(CpuVendor::Unknown.iommu_params(), &["iommu=pt"]);
    }

    #[test]
    fn an_unknown_package_manager_has_no_install_command() {
        assert!(PackageManager::Unknown.install(&["qemu-full"]).is_none());
        let p = PackageManager::Pacman
            .install(&["libvirt", "qemu-full"])
            .unwrap();
        assert!(p.contains("--needed"), "a re-run must be free: {p}");
        assert!(p.contains("--noconfirm"));
        let a = PackageManager::Apt
            .install(&["libvirt-daemon-system"])
            .unwrap();
        assert!(
            a.contains("DEBIAN_FRONTEND=noninteractive"),
            "apt must not prompt: {a}"
        );
    }

    /// `ready_for_guests` gates the whole UI, so it must require every piece a
    /// Windows guest actually needs. OVMF is the one that gets forgotten: with
    /// qemu, libvirt and KVM all present and no firmware, a Windows space
    /// defines fine and then refuses to boot with nothing in the log about why.
    #[test]
    fn readiness_requires_firmware_not_just_a_daemon() {
        let base = Facts {
            kvm_available: true,
            libvirt_running: true,
            qemu_installed: true,
            ovmf_installed: true,
            ..probe_blank(&Host::Local)
        };
        assert!(base.ready_for_guests());
        assert!(!Facts {
            ovmf_installed: false,
            ..base.clone()
        }
        .ready_for_guests());
        assert!(!Facts {
            kvm_available: false,
            ..base.clone()
        }
        .ready_for_guests());
        assert!(!Facts {
            libvirt_running: false,
            ..base.clone()
        }
        .ready_for_guests());
    }

    /// Every probe line must be a command the shell can actually run.
    ///
    /// This exists because `grep -qmE1` shipped: the bundled flags made grep
    /// read "E1" as the argument to -m, so it exited 2 for *invalid usage*,
    /// and the `|| p virt_supported 0` dutifully recorded that as "this CPU
    /// cannot virtualise". Every host in the world would have been reported
    /// as unable to run a guest — the one fact that makes the whole package
    /// look inapplicable — and nothing anywhere would have said why.
    ///
    /// Running the real probe against the real machine is the only check that
    /// catches it: the string is syntactically fine, and a mocked shell would
    /// have accepted it too.
    #[tokio::test]
    async fn the_probe_runs_without_a_usage_error() {
        let out = Host::Local
            .run(PROBE)
            .await
            .expect("the probe should at least run");

        // Exit-2-style complaints land on stderr. A probe line that cannot be
        // parsed says so there, and nowhere else.
        let stderr = out.stderr.to_ascii_lowercase();
        for complaint in [
            "invalid argument",
            "invalid option",
            "unrecognized option",
            "usage:",
        ] {
            assert!(
                !stderr.contains(complaint),
                "a probe line is malformed ({complaint}):\n{}",
                out.stderr
            );
        }

        // And the facts it produced have to be internally consistent. A host
        // the kernel gave IOMMU groups to is a host whose CPU virtualises —
        // the exact contradiction the bug produced.
        let f = probe(&Host::Local).await.unwrap();
        if f.iommu_groups > 0 {
            assert!(
                f.virt_supported,
                "the kernel exposed {} IOMMU groups, so the CPU must support virtualisation — \
                 reporting otherwise means a probe line failed silently",
                f.iommu_groups
            );
        }
    }

    /// An unreachable host must not read as a host with no CPU and no RAM.
    #[test]
    fn an_unreachable_host_says_so_rather_than_showing_zeroes() {
        let f = probe_blank(&Host::Local);
        assert!(!f.reachable);
        assert_eq!(f.status_label(), "Unreachable");
        assert_eq!(f.status_color(), "#ff453a");
    }

    /// Arbitrary shell must never reach the privileged path. The wrapper's
    /// verb set is the whole reason one sudoers rule naming one program is a
    /// narrow grant rather than a root shell, so `wrap` only ever composes
    /// the wrapper with a verb.
    #[test]
    fn privilege_wraps_only_the_wrapper() {
        assert_eq!(
            Privilege::Wrapper.wrap("install-packages").unwrap(),
            "sudo -n /usr/local/bin/leo-spaces-admin install-packages"
        );
        // Already root: no sudo in the path at all.
        assert_eq!(
            Privilege::Root.wrap("enable-daemon").unwrap(),
            "/usr/local/bin/leo-spaces-admin enable-daemon"
        );
        // Nothing to wrap with means nothing is claimed to be runnable.
        assert!(Privilege::None.wrap("install-packages").is_none());
        assert!(!Privilege::None.can_apply());
        assert!(Privilege::Wrapper.can_apply());
    }

    /// A wrapper on disk with no sudoers rule is the state a half-finished
    /// enable leaves. It must not read as usable, or every apply fails with a
    /// password prompt into a closed stdin.
    #[test]
    fn a_wrapper_without_its_rule_is_not_privilege() {
        let f = Facts {
            reachable: true,
            wrapper_installed: true,
            wrapper_usable: false,
            privilege: Privilege::None,
            ..probe_blank(&Host::Local)
        };
        assert!(!f.privilege.can_apply());
    }

    #[test]
    fn status_distinguishes_not_installed_from_half_installed() {
        let nothing = Facts {
            reachable: true,
            virt_supported: true,
            ..probe_blank(&Host::Local)
        };
        assert_eq!(nothing.status_label(), "Not installed");

        let half = Facts {
            libvirt_installed: true,
            ..nothing.clone()
        };
        assert_eq!(half.status_label(), "Needs setup");

        let no_virt = Facts {
            virt_supported: false,
            ..nothing.clone()
        };
        assert_eq!(no_virt.status_label(), "No virtualisation");
    }
}
