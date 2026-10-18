//! Turning a bare Linux box into a hypervisor, one named step at a time.
//!
//! The plan is **derived from [`Facts`], never stored**. A stored checklist
//! goes stale the moment someone installs a package by hand, and then the page
//! offers to do work that is already done — which is how an operator learns to
//! stop trusting it. Re-deriving on every read means a step disappears because
//! it became true, whichever way it became true.
//!
//! Every step carries the command it would run, whether or not this process
//! can run it. That is deliberate: on a box with no privilege the plan is
//! still the deliverable, and "here is exactly what to paste" beats "something
//! is missing".

use serde::Serialize;

use crate::facts::{Bootloader, Facts, PackageManager, WRAPPER};

/// Where space disks, firmware vars and installer ISOs live.
pub const STATE_DIR: &str = "/var/lib/leo-spaces";

/// The libvirt storage pool this package owns.
pub const POOL: &str = "leo-spaces";

/// How much damage a step could do if it went wrong.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Risk {
    /// Installs software or starts a daemon. Reversible, no reboot.
    Safe,
    /// Changes how the box boots. A mistake here is a machine that needs a
    /// console to recover, so these are never bundled into "set everything up".
    Boot,
}

/// One thing that has to be true, and how to make it true.
#[derive(Debug, Clone, Serialize)]
pub struct Step {
    /// Stable id. The apply path takes ids, never commands — so the set of
    /// things that can be run is exactly the set this module generates.
    pub id: String,
    pub title: String,
    /// Why it matters, in a sentence an operator can act on.
    pub detail: String,
    /// True when the host already satisfies it.
    pub satisfied: bool,
    /// The shell this step would run, as the host would see it. `None` when
    /// the host is too unfamiliar to say — an unknown package manager, a
    /// bootloader nothing here knows how to edit.
    pub command: Option<String>,
    /// The wrapper verb this step maps to, when it needs privilege.
    pub verb: Option<String>,
    pub risk: Risk,
    pub needs_reboot: bool,
    /// The host is configured for this step but has not acted on it yet — a
    /// boot-time change written and waiting for the reboot. Distinct from
    /// `satisfied` (the host already does it) and from pending (nothing has
    /// been written). Without it, "apply, reboot, still not done" and "never
    /// applied" drew the same row.
    pub applied: bool,
    /// Steps that must be satisfied first. Applying out of order is refused
    /// rather than attempted — `virsh net-start` before libvirt is installed
    /// fails in a way that reads like a broken network, not a missed step.
    pub requires: Vec<String>,
    /// Why this step cannot be run here, when it cannot.
    pub blocked: Option<String>,
}

impl Step {
    /// Whether this process could actually apply it right now.
    pub fn applicable(&self) -> bool {
        !self.satisfied && self.command.is_some() && self.blocked.is_none()
    }
}

/// The whole plan, plus the one-line verdict the UI leads with.
#[derive(Debug, Clone, Serialize)]
pub struct Plan {
    pub steps: Vec<Step>,
    /// Steps that are neither satisfied nor blocked — what "Set up" would do.
    pub pending: usize,
    /// Satisfied steps, for the "7 of 9" reading.
    pub satisfied: usize,
    /// Steps that cannot proceed on this host, with reasons.
    pub blocked: usize,
    /// True when every non-boot step is satisfied.
    pub guests_ready: bool,
    /// What the operator has to do that this package cannot, if anything.
    pub handover: Option<String>,
}

/// The packages a hypervisor needs, per distro.
///
/// Chosen to make a **Windows** guest boot unattended, which is a longer list
/// than a Linux guest needs:
/// - `edk2-ovmf`/`ovmf` — UEFI firmware. Windows 11 will not install on SeaBIOS.
/// - `swtpm` — a software TPM 2.0. Windows 11 setup refuses the disk without one.
/// - `dnsmasq` — what libvirt's `default` NAT network uses for DHCP.
/// - `virt-install` — how a space gets defined without hand-writing domain XML.
fn packages(pm: PackageManager) -> &'static [&'static str] {
    match pm {
        PackageManager::Pacman => &[
            "qemu-full",
            "libvirt",
            "virt-install",
            "edk2-ovmf",
            "swtpm",
            "dnsmasq",
            "dmidecode",
        ],
        PackageManager::Apt => &[
            "qemu-system-x86",
            "qemu-utils",
            "libvirt-daemon-system",
            "libvirt-clients",
            "virtinst",
            "ovmf",
            "swtpm",
            "swtpm-tools",
            "dnsmasq-base",
            "dmidecode",
        ],
        PackageManager::Dnf => &[
            "qemu-kvm",
            "libvirt",
            "virt-install",
            "edk2-ovmf",
            "swtpm",
            "swtpm-tools",
            "dnsmasq",
            "dmidecode",
        ],
        PackageManager::Unknown => &[],
    }
}

/// Derive the plan.
pub fn plan(f: &Facts) -> Plan {
    let mut steps = Vec::new();
    let p = f.privilege;

    // ── A host that cannot virtualise at all ────────────────────────────────
    //
    // Reported as a step so it appears in the same list as everything else,
    // and marked blocked because no command fixes it. The alternative — an
    // empty plan — reads as "nothing to do", which is the opposite of true.
    if f.reachable && !f.virt_supported {
        steps.push(Step {
            id: "virt".into(),
            title: "Hardware virtualisation".into(),
            detail: format!(
                "{} reports no SVM/VMX support. Either it is disabled in firmware — look for SVM, AMD-V, VT-x or \"Intel Virtualization Technology\" — or this box is itself a guest without nested virtualisation.",
                if f.cpu_model.is_empty() { "The CPU" } else { &f.cpu_model }
            ),
            satisfied: false,
            command: None,
            verb: None,
            risk: Risk::Safe,
            needs_reboot: false,
            applied: false,
            requires: vec![],
            blocked: Some("enable virtualisation in the firmware setup, then reboot".into()),
        });
    }

    // ── Packages ────────────────────────────────────────────────────────────
    let pkgs = packages(f.package_manager);
    let pkg_satisfied =
        f.qemu_installed && f.libvirt_installed && f.ovmf_installed && f.swtpm_installed;
    steps.push(Step {
        id: "packages".into(),
        title: "Hypervisor packages".into(),
        detail: if pkgs.is_empty() {
            "No package manager recognised, so the package list cannot be named for this distribution. QEMU, libvirt, OVMF and swtpm are what a Windows guest needs.".into()
        } else {
            format!(
                "QEMU, libvirt, OVMF firmware and a software TPM. The last two are what make a Windows 11 guest installable: it refuses SeaBIOS and refuses a disk with no TPM 2.0. Installs: {}.",
                pkgs.join(", ")
            )
        },
        satisfied: pkg_satisfied,
        command: if pkgs.is_empty() { None } else { p.wrap("install-packages").or_else(|| f.package_manager.install(pkgs)) },
        verb: Some("install-packages".into()),
        risk: Risk::Safe,
        needs_reboot: false,
        applied: false,
        requires: vec![],
        blocked: if pkgs.is_empty() {
            Some(format!("unrecognised distribution ({})", if f.os_id.is_empty() { "unknown" } else { &f.os_id }))
        } else if !p.can_apply() {
            Some(privilege_handover(f))
        } else {
            None
        },
    });

    // ── The daemon ──────────────────────────────────────────────────────────
    steps.push(Step {
        id: "daemon".into(),
        title: "libvirt service".into(),
        detail: "Enables and starts libvirt so it survives a reboot. Socket activation is preferred, with the modular daemons as the fallback — Arch ships the monolithic libvirtd and Debian has moved to virtqemud, and a host with either is a working host.".into(),
        satisfied: f.libvirt_running,
        command: p.wrap("enable-daemon"),
        verb: Some("enable-daemon".into()),
        risk: Risk::Safe,
        needs_reboot: false,
        applied: false,
        requires: vec!["packages".into()],
        blocked: blocked_by_privilege(f),
    });

    // ── Guest networking ────────────────────────────────────────────────────
    steps.push(Step {
        id: "network".into(),
        title: "Guest network".into(),
        detail: "Defines and autostarts libvirt's `default` NAT network, which is how a guest gets an address and reaches the internet with no bridge to configure. A space needs this before it can be given a NIC.".into(),
        satisfied: f.default_network_active,
        command: p.wrap("setup-network"),
        verb: Some("setup-network".into()),
        risk: Risk::Safe,
        needs_reboot: false,
        applied: false,
        requires: vec!["daemon".into()],
        blocked: blocked_by_privilege(f),
    });

    // ── The host firewall, opened for guests ────────────────────────────
    steps.push(Step {
        id: "guest-net".into(),
        title: "Guest network through the firewall".into(),
        detail: "Lets guests on the `default` network get an address from the host and reach the internet. Leo OS's firewall drops both by default \u{2014} a Windows guest then reports an unidentified network with no internet \u{2014} so this inserts accept rules for the guest bridge ahead of its drops, and a unit re-applies them whenever the firewall reloads.".into(),
        satisfied: f.guest_net_open,
        command: p.wrap("open-guest-network"),
        verb: Some("open-guest-network".into()),
        risk: Risk::Safe,
        needs_reboot: false,
        applied: false,
        requires: vec!["network".into()],
        blocked: blocked_by_privilege(f),
    });

    // ── Storage ─────────────────────────────────────────────────────────────
    //
    // Satisfaction IS probed, and the comment that used to sit here said why
    // while the code did the opposite: it read "reporting it as pending on a
    // host where it is already there would be a lie" above a hardcoded
    // `satisfied: false`. The probe it promised (`pool_ready`, riding along
    // with the libvirt checks) now exists.
    //
    // It was never only cosmetic: `virtio` requires `storage`, so a step that
    // could never be satisfied left virtio permanently unapplicable and the
    // setup list unable to reach a finished state — `apply virtio` answered
    // "needs storage applied first" immediately after storage had succeeded.
    steps.push(Step {
        id: "storage".into(),
        title: "Space storage".into(),
        detail: format!(
            "Creates {STATE_DIR} for disks, firmware variables and installer media, and registers it with libvirt as the `{POOL}` pool. Keeping it out of /var/lib/libvirt means this package's disks are identifiable as its own."
        ),
        satisfied: f.pool_ready,
        command: p.wrap("setup-storage"),
        verb: Some("setup-storage".into()),
        risk: Risk::Safe,
        needs_reboot: false,
        applied: false,
        requires: vec!["daemon".into()],
        blocked: blocked_by_privilege(f),
    });

    // ── Group membership ────────────────────────────────────────────────────
    steps.push(Step {
        id: "group".into(),
        title: "libvirt access".into(),
        detail: "Adds this service account to the `libvirt` group so it can manage guests without root for every call. Group membership is read at login, so the hub service has to restart before it takes effect.".into(),
        satisfied: f.in_libvirt_group,
        command: p.wrap("add-group"),
        verb: Some("add-group".into()),
        risk: Risk::Safe,
        needs_reboot: false,
        applied: false,
        requires: vec!["packages".into()],
        blocked: blocked_by_privilege(f),
    });

    // ── Windows virtio drivers ──────────────────────────────────────────────
    steps.push(Step {
        id: "virtio".into(),
        title: "Windows virtio drivers".into(),
        detail: "Downloads the signed virtio-win driver ISO. Windows setup cannot see a virtio disk without it — the installer reaches the \"where do you want to install\" screen and lists nothing, which is the single most common way a Windows guest fails for a reason that looks like broken storage.".into(),
        // Probed like storage is: the same hardcoded `false` sat here, so the
        // ISO could be on disk and the step still read Pending.
        satisfied: f.virtio_iso_present,
        command: p.wrap("fetch-virtio"),
        verb: Some("fetch-virtio".into()),
        risk: Risk::Safe,
        needs_reboot: false,
        applied: false,
        requires: vec!["storage".into()],
        blocked: blocked_by_privilege(f),
    });

    // ── IOMMU ───────────────────────────────────────────────────────────────
    //
    // Everything above is reversible with one command. This is not: it changes
    // how the box boots, and the next boot is where a mistake shows up. So it
    // is a separate step, marked Risk::Boot, excluded from "set up everything",
    // and the wrapper writes a backup before touching anything.
    let iommu_params = f.cpu_vendor.iommu_params().join(" ");
    steps.push(Step {
        id: "iommu".into(),
        title: "IOMMU".into(),
        detail: format!(
            "Adds `{iommu_params}` to the kernel command line via {}. Required before any device can be isolated for one guest — without it the kernel exposes no IOMMU groups and passthrough is unavailable rather than unconfigured. A backup of the boot configuration is written first, and this needs a reboot.",
            match f.bootloader {
                Bootloader::SystemdBoot => "the systemd-boot entries",
                Bootloader::Grub => "/etc/default/grub",
                Bootloader::Unknown => "the bootloader",
            }
        ),
        satisfied: f.iommu_enabled(),
        command: match f.bootloader {
            Bootloader::Unknown => None,
            _ => p.wrap("enable-iommu"),
        },
        verb: Some("enable-iommu".into()),
        risk: Risk::Boot,
        needs_reboot: true,
        applied: false,
        requires: vec![],
        blocked: match f.bootloader {
            Bootloader::Unknown => Some(
                "no editable bootloader configuration found — add the parameters however this box sets its kernel command line".into(),
            ),
            _ => blocked_by_privilege(f),
        },
    });

    // ── GPU passthrough ─────────────────────────────────────────────────────
    //
    // Only offered for a card that can actually be handed over. A blocked
    // candidate still produces a step, because the reason is the useful part.
    for gpu in f.gpus.iter().filter(|g| !g.integrated) {
        let blockers = gpu.passthrough_blockers(&f.gpus);
        // The whole card, every function: a group with one function still
        // on its host driver is "not viable" and the guest refuses to start
        // — which is exactly how the A4000's HDMI audio stopped a guest.
        let bound = gpu.fully_bound();
        let ids = gpu.all_ids().join(",");
        let configured = gpu
            .all_ids()
            .iter()
            .all(|id| f.vfio_ids.iter().any(|i| i == id));
        // Three states past "nothing written", and the row has to say which:
        // the config is in and a reboot is what is left; or the reboot
        // happened and the card is still where it was, which is a failed
        // binding and not a step somebody forgot.
        let standing = if bound || !configured {
            String::new()
        } else if f.vfio_config_predates_boot {
            format!(
                " Configured, but after a reboot the card is not vfio-pci's: {} — the binding did not take. Apply again: the helper carries vfio_pci into the initramfs itself and checks that it did.",
                gpu.held_functions().join(", ")
            )
        } else {
            " Configured — reboot to let vfio-pci claim it.".to_string()
        };
        steps.push(Step {
            id: format!("vfio:{}", gpu.address),
            title: format!("Pass through {}", gpu.short_name()),
            detail: format!(
                "Binds {} ({}) to vfio-pci at boot so one space can own it outright. {} Needs a reboot.{standing}",
                gpu.all_addresses().join(" + "),
                ids,
                // Said in terms of OTHER devices. A card's own audio function
                // is in the group and goes with it, which is correct and not
                // worth alarming anybody about.
                if gpu.foreign_siblings().is_empty() {
                    "Nothing else of the host's is in its IOMMU group, so only the card goes."
                        .to_string()
                } else {
                    format!(
                        "Its IOMMU group also holds {}, which VFIO claims together with it.",
                        gpu.foreign_siblings().join(", ")
                    )
                }
            ),
            satisfied: bound,
            command: p.wrap(&format!("bind-vfio {ids}")),
            verb: Some(format!("bind-vfio {ids}")),
            risk: Risk::Boot,
            needs_reboot: true,
            applied: configured && !bound,
            requires: vec!["iommu".into()],
            blocked: if !blockers.is_empty() {
                Some(blockers.join("; "))
            } else {
                blocked_by_privilege(f)
            },
        });
    }

    let satisfied = steps.iter().filter(|s| s.satisfied).count();
    let blocked = steps
        .iter()
        .filter(|s| !s.satisfied && s.blocked.is_some())
        .count();
    let pending = steps.iter().filter(|s| s.applicable()).count();

    // "Ready for guests" deliberately ignores the boot steps: a host with no
    // GPU passed through still runs Windows spaces perfectly well, software
    // rendered. Folding them in would report a working hypervisor as unfinished.
    let guests_ready = f.ready_for_guests();

    Plan {
        steps,
        pending,
        satisfied,
        blocked,
        guests_ready,
        handover: if f.reachable && !f.privilege.can_apply() {
            Some(privilege_handover(f))
        } else {
            None
        },
    }
}

/// The step-level reason when the only thing missing is privilege.
fn blocked_by_privilege(f: &Facts) -> Option<String> {
    if f.privilege.can_apply() {
        None
    } else {
        Some(privilege_handover(f))
    }
}

/// What an admin has to do once, for this package to be able to act.
///
/// Named precisely rather than described, because the gap between "you need
/// more privileges" and a command somebody can paste is the entire difference
/// between a package that works and one that gets uninstalled.
pub fn privilege_handover(f: &Facts) -> String {
    if f.wrapper_installed {
        format!(
            "{WRAPPER} is installed but not permitted — the sudoers rule for it is missing. Re-run `sudo bash scripts/install-admin-wrapper.sh` to restore it."
        )
    } else {
        format!(
            "Hypervisor setup is root work. Run `sudo bash scripts/install-admin-wrapper.sh` once on {} to install {WRAPPER} and the single sudoers rule that permits it; everything here becomes applicable from the UI after that.",
            if f.hostname.is_empty() { "the host" } else { &f.hostname }
        )
    }
}

/// Look up a step by id in a freshly derived plan.
///
/// The apply path exists only in terms of this: a caller names a step, and the
/// command that runs is the one this module generated for the host as it is
/// right now. There is no path from caller text to a shell.
pub fn step<'a>(plan: &'a Plan, id: &str) -> Option<&'a Step> {
    plan.steps.iter().find(|s| s.id == id)
}

/// Which prerequisites of a step are not yet satisfied.
pub fn unmet_requirements<'a>(plan: &'a Plan, s: &Step) -> Vec<&'a str> {
    s.requires
        .iter()
        .filter_map(|r| plan.steps.iter().find(|c| &c.id == r))
        .filter(|c| !c.satisfied)
        .map(|c| c.id.as_str())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts::{CpuVendor, Gpu, Privilege};
    use crate::host::Host;

    /// **A pool that exists is Done, and virtio becomes reachable.**
    ///
    /// `storage` used to be hardcoded `satisfied: false`, so it could never
    /// complete — and `virtio` requires it, so `apply virtio` answered "needs
    /// storage applied first" even immediately after storage had succeeded.
    /// The setup list could not reach a finished state on any host. Measured
    /// on leo-virt: the pool was `active / running / 1.82 TiB` while the plan
    /// still called the step pending.
    #[test]
    fn a_running_pool_satisfies_storage_and_unblocks_virtio() {
        let mut f = work_hub();
        f.qemu_installed = true;
        f.libvirt_installed = true;
        f.ovmf_installed = true;
        f.swtpm_installed = true;
        f.libvirt_running = true;

        // Without the pool, storage is the thing left to do and virtio waits.
        f.pool_ready = false;
        let p = plan(&f);
        let storage = step(&p, "storage").expect("a storage step");
        assert!(!storage.satisfied, "no pool, so storage is not done");
        assert!(
            !unmet_requirements(&p, step(&p, "virtio").expect("a virtio step")).is_empty(),
            "virtio must wait for storage while the pool is absent"
        );

        // With it, storage is done and virtio is free to run.
        f.pool_ready = true;
        let p = plan(&f);
        let storage = step(&p, "storage").expect("a storage step");
        assert!(
            storage.satisfied,
            "a running pool must satisfy storage — hardcoding false is what left \
             virtio permanently unapplicable"
        );
        assert!(
            unmet_requirements(&p, step(&p, "virtio").expect("a virtio step")).is_empty(),
            "with the pool up, virtio has nothing left to wait for"
        );
        assert!(
            !step(&p, "virtio").expect("a virtio step").satisfied,
            "no ISO on disk, so virtio is not done"
        );

        // And with the ISO on disk, virtio is done too.
        f.virtio_iso_present = true;
        let p = plan(&f);
        assert!(
            step(&p, "virtio").expect("a virtio step").satisfied,
            "a present ISO must satisfy virtio — the same hardcoded false sat under this step"
        );
    }

    /// A card is several PCI functions in one IOMMU group, and VFIO claims
    /// the group: the step binds and reports every function, so a guest is
    /// never refused with "group is not viable" over an HDMI audio device.
    #[test]
    fn the_step_binds_every_function_of_the_card() {
        use crate::facts::Function;
        let mut f = work_hub();
        f.gpus.push(crate::facts::Gpu {
            address: "0000:01:00.0".into(),
            ids: "10de:24b0".into(),
            name: "NVIDIA Corporation GA104GL [RTX A4000]".into(),
            driver: "vfio-pci".into(),
            iommu_group: Some(29),
            group_siblings: vec!["0000:01:00.1".into()],
            is_boot_vga: false,
            integrated: false,
            functions: vec![Function {
                address: "0000:01:00.1".into(),
                ids: "10de:228b".into(),
                driver: "snd_hda_intel".into(),
            }],
        });
        f.vfio_ids = vec!["10de:24b0".into()];
        f.vfio_config_predates_boot = true;
        let p = plan(&f);
        let s = step(&p, "vfio:0000:01:00.0").expect("the step");
        assert!(
            !s.satisfied,
            "the audio function is still the host's, so the card is not passable yet"
        );
        assert_eq!(s.verb.as_deref(), Some("bind-vfio 10de:24b0,10de:228b"));
        assert!(!s.applied, "only half the ids are configured");
        assert!(
            s.detail.contains("0000:01:00.0 + 0000:01:00.1"),
            "{}",
            s.detail
        );

        f.gpus[1].functions[0].driver = "vfio-pci".into();
        let p = plan(&f);
        assert!(step(&p, "vfio:0000:01:00.0").unwrap().satisfied);
    }

    /// A card `bind-vfio` has been run for is three different rows depending
    /// on what the host did with it: written and waiting for a reboot;
    /// rebooted and still held by its own driver, which is a binding that did
    /// not take; or claimed, which is done. The first two drew identically
    /// until a real reboot changed nothing and the row said so in no way.
    #[test]
    fn a_configured_card_is_applied_until_the_reboot_and_says_when_it_did_not_take() {
        let mut f = work_hub();
        f.gpus.push(crate::facts::Gpu {
            address: "0000:01:00.0".into(),
            ids: "10de:24b0".into(),
            name: "NVIDIA Corporation GA104GL [RTX A4000]".into(),
            driver: "nvidia".into(),
            iommu_group: Some(29),
            group_siblings: vec!["0000:01:00.1".into()],
            is_boot_vga: false,
            integrated: false,
            functions: vec![],
        });
        let id = "vfio:0000:01:00.0";

        // Nothing written: pending, and the detail says nothing about state.
        let p = plan(&f);
        let s = step(&p, id).expect("a passthrough step");
        assert!(!s.satisfied && !s.applied, "{s:?}");
        assert!(!s.detail.contains("Configured"), "{}", s.detail);

        // Written, no reboot yet: applied, and the row says to reboot.
        f.vfio_ids = vec!["10de:24b0".into()];
        f.vfio_config_predates_boot = false;
        let p = plan(&f);
        let s = step(&p, id).expect("a passthrough step");
        assert!(s.applied && !s.satisfied, "{s:?}");
        assert!(
            s.detail.contains("reboot to let vfio-pci claim it"),
            "{}",
            s.detail
        );

        // Rebooted, still nvidia's: applied, and the row says it did not take
        // and names the holder.
        f.vfio_config_predates_boot = true;
        let p = plan(&f);
        let s = step(&p, id).expect("a passthrough step");
        assert!(s.applied && !s.satisfied, "{s:?}");
        assert!(s.detail.contains("did not take"), "{}", s.detail);
        assert!(s.detail.contains("0000:01:00.0 (nvidia)"), "{}", s.detail);

        // Claimed: done, and no longer "applied" — that word is for waiting.
        // (The fixture's integrated GPU sits at index 0; the card is last.)
        f.gpus.last_mut().unwrap().driver = "vfio-pci".into();
        let p = plan(&f);
        let s = step(&p, id).expect("a passthrough step");
        assert!(s.satisfied && !s.applied, "{s:?}");

        // A different card's id in the config is not this card configured.
        f.gpus.last_mut().unwrap().driver = "nvidia".into();
        f.vfio_ids = vec!["1002:73df".into()];
        let p = plan(&f);
        assert!(!step(&p, id).expect("a passthrough step").applied);
    }

    /// A host shaped like the work hub: Arch, AMD, systemd-boot, nothing
    /// installed, integrated graphics only, and the wrapper in place.
    fn work_hub() -> Facts {
        let mut f = crate::facts::probe_blank(&Host::Local);
        f.reachable = true;
        f.hostname = "leo-work".into();
        f.os = "Arch Linux".into();
        f.os_id = "arch".into();
        f.package_manager = PackageManager::Pacman;
        f.bootloader = Bootloader::SystemdBoot;
        f.cpu_model = "AMD Ryzen 9 9950X 16-Core Processor".into();
        f.cpu_vendor = CpuVendor::Amd;
        f.cpu_threads = 32;
        f.memory_mb = 93_000;
        f.virt_supported = true;
        f.kvm_available = true;
        f.privilege = Privilege::Wrapper;
        f.wrapper_installed = true;
        f.wrapper_usable = true;
        f.gpus = vec![Gpu {
            address: "0000:78:00.0".into(),
            ids: "1002:13c0".into(),
            name: "VGA compatible controller: Advanced Micro Devices, Inc. [AMD/ATI] Granite Ridge [Radeon Graphics]".into(),
            driver: "amdgpu".into(),
            iommu_group: None,
            group_siblings: vec![],
            is_boot_vga: true,
            integrated: true,
            functions: vec![],
        }];
        f
    }

    /// The plan has to be derivable with nothing installed, and every step has
    /// to be nameable. A plan that comes back empty on a bare host is the one
    /// outcome that makes the page useless.
    #[test]
    fn a_bare_host_gets_a_full_plan() {
        let p = plan(&work_hub());
        let ids: Vec<&str> = p.steps.iter().map(|s| s.id.as_str()).collect();
        for want in [
            "packages", "daemon", "network", "storage", "group", "virtio", "iommu",
        ] {
            assert!(ids.contains(&want), "plan should include {want}: {ids:?}");
        }
        assert!(!p.guests_ready);
        assert_eq!(p.satisfied, 0);
        assert!(p.pending > 0);
    }

    /// The integrated display must not be offered for passthrough. Taking the
    /// 9950X's own graphics away from the host is a reboot to a dark console,
    /// and on the work hub it is the *only* display.
    #[test]
    fn integrated_graphics_are_not_offered_for_passthrough() {
        let p = plan(&work_hub());
        assert!(
            !p.steps.iter().any(|s| s.id.starts_with("vfio:")),
            "integrated graphics must not produce a passthrough step"
        );
    }

    /// A discrete card does get a step, and when its group is dirty the step
    /// says which devices would go with it rather than just refusing.
    #[test]
    fn a_discrete_card_gets_a_step_that_names_its_group() {
        let mut f = work_hub();
        f.iommu_groups = 40;
        f.gpus.push(Gpu {
            address: "0000:01:00.0".into(),
            ids: "10de:24b0".into(),
            name: "VGA compatible controller: NVIDIA Corporation GA104GL [RTX A4000]".into(),
            driver: "nvidia".into(),
            iommu_group: Some(15),
            group_siblings: vec!["0000:00:1f.2".into()],
            is_boot_vga: false,
            integrated: false,
            functions: vec![],
        });
        let p = plan(&f);
        let s = step(&p, "vfio:0000:01:00.0").expect("discrete card should get a step");
        assert_eq!(s.title, "Pass through RTX A4000");
        assert!(s.needs_reboot);
        assert_eq!(s.risk, Risk::Boot);
        // The sibling appears in both the blocker and the explanation, because
        // the operator's decision depends on knowing what else moves.
        assert!(s.blocked.as_deref().unwrap().contains("0000:00:1f.2"));
        assert!(s.detail.contains("0000:00:1f.2"));
        assert!(!s.applicable(), "a dirty group must not be applicable");
    }

    /// A clean discrete card on an IOMMU-enabled host is applicable.
    #[test]
    fn a_clean_discrete_card_is_applicable() {
        let mut f = work_hub();
        f.iommu_groups = 40;
        f.gpus.push(Gpu {
            address: "0000:01:00.0".into(),
            ids: "10de:24b0".into(),
            name: "NVIDIA GA104GL [RTX A4000]".into(),
            driver: "nvidia".into(),
            iommu_group: Some(15),
            group_siblings: vec![],
            is_boot_vga: false,
            integrated: false,
            functions: vec![],
        });
        let p = plan(&f);
        let s = step(&p, "vfio:0000:01:00.0").unwrap();
        assert!(s.applicable(), "blocked: {:?}", s.blocked);
        assert!(s
            .command
            .as_deref()
            .unwrap()
            .contains("bind-vfio 10de:24b0"));
    }

    /// The boot-touching steps must be distinguishable from the rest, because
    /// "set everything up" must never include them.
    #[test]
    fn boot_steps_are_marked_and_separable() {
        let p = plan(&work_hub());
        let iommu = step(&p, "iommu").unwrap();
        assert_eq!(iommu.risk, Risk::Boot);
        assert!(iommu.needs_reboot);

        for id in [
            "packages", "daemon", "network", "storage", "group", "virtio",
        ] {
            let s = step(&p, id).unwrap();
            assert_eq!(s.risk, Risk::Safe, "{id} should be safe");
            assert!(!s.needs_reboot, "{id} should not need a reboot");
        }
    }

    /// Without privilege, every step still names its command — the plan is the
    /// deliverable on a box this package cannot touch — and the handover says
    /// exactly what to run.
    #[test]
    fn without_privilege_the_plan_still_hands_over_commands() {
        let mut f = work_hub();
        f.privilege = Privilege::None;
        f.wrapper_installed = false;
        f.wrapper_usable = false;

        let p = plan(&f);
        assert!(p
            .handover
            .as_deref()
            .unwrap()
            .contains("install-admin-wrapper.sh"));
        assert!(
            p.handover.as_deref().unwrap().contains("leo-work"),
            "should name the host"
        );

        // The package step falls back to the distro's own install command, so
        // there is something to paste even with no wrapper at all.
        let pkg = step(&p, "packages").unwrap();
        let cmd = pkg.command.as_deref().expect("a command to paste");
        assert!(
            cmd.contains("pacman -S"),
            "should fall back to pacman: {cmd}"
        );
        assert!(cmd.contains("edk2-ovmf"), "Windows needs OVMF: {cmd}");
        assert!(cmd.contains("swtpm"), "Windows 11 needs a TPM: {cmd}");
        // But it is not applicable, because this process cannot run it.
        assert!(!pkg.applicable());
        assert_eq!(
            p.pending, 0,
            "nothing is pending when nothing can be applied"
        );
    }

    /// A wrapper on disk with no rule gets its own message — "install it" is
    /// wrong advice when it is already installed.
    #[test]
    fn a_wrapper_missing_its_rule_says_so() {
        let mut f = work_hub();
        f.privilege = Privilege::None;
        f.wrapper_installed = true;
        f.wrapper_usable = false;
        let p = plan(&f);
        let h = p.handover.unwrap();
        assert!(h.contains("not permitted"), "{h}");
        assert!(h.contains("sudoers"), "{h}");
    }

    /// A CPU with no virtualisation gets a step that explains where to look,
    /// not an empty plan that reads as "nothing to do".
    #[test]
    fn no_virtualisation_support_is_reported_as_a_blocked_step() {
        let mut f = work_hub();
        f.virt_supported = false;
        let p = plan(&f);
        let s = step(&p, "virt").expect("should name the problem");
        assert!(s.blocked.is_some());
        assert!(s.command.is_none(), "nothing can fix this from software");
        assert!(
            s.detail.contains("AMD-V"),
            "should say what to look for: {}",
            s.detail
        );
        assert!(
            s.detail.contains("9950X"),
            "should name the CPU: {}",
            s.detail
        );
    }

    /// An unrecognised distro is blocked with its id named, and crucially does
    /// not produce a half-right command for a package manager it doesn't have.
    #[test]
    fn an_unknown_distro_is_blocked_rather_than_guessed_at() {
        let mut f = work_hub();
        f.package_manager = PackageManager::Unknown;
        f.os_id = "voidlinux".into();
        let p = plan(&f);
        let s = step(&p, "packages").unwrap();
        assert!(s.command.is_none(), "must not invent an install command");
        assert!(s.blocked.as_deref().unwrap().contains("voidlinux"));
    }

    /// Ordering is enforced, not hoped for. `virsh net-start` before libvirt
    /// exists fails in a way that reads like a broken network.
    #[test]
    fn prerequisites_are_reported_as_unmet() {
        let p = plan(&work_hub());
        let net = step(&p, "network").unwrap();
        assert_eq!(net.requires, vec!["daemon"]);
        assert_eq!(unmet_requirements(&p, net), vec!["daemon"]);

        let vfio_requires_iommu = plan(&{
            let mut f = work_hub();
            f.iommu_groups = 0;
            f.gpus.push(Gpu {
                address: "0000:01:00.0".into(),
                ids: "10de:24b0".into(),
                name: "[RTX A4000]".into(),
                driver: "nvidia".into(),
                iommu_group: Some(15),
                group_siblings: vec![],
                is_boot_vga: false,
                integrated: false,
                functions: vec![],
            });
            f
        });
        let g = step(&vfio_requires_iommu, "vfio:0000:01:00.0").unwrap();
        assert_eq!(unmet_requirements(&vfio_requires_iommu, g), vec!["iommu"]);
    }

    /// A fully provisioned host reports ready and offers nothing it has
    /// already done — the property that keeps the page trustworthy when
    /// somebody installed half of it by hand.
    #[test]
    fn a_provisioned_host_offers_only_what_is_left() {
        let mut f = work_hub();
        f.qemu_installed = true;
        f.libvirt_installed = true;
        f.ovmf_installed = true;
        f.swtpm_installed = true;
        f.libvirt_running = true;
        f.default_network_active = true;
        f.in_libvirt_group = true;

        let p = plan(&f);
        assert!(p.guests_ready);
        assert!(step(&p, "packages").unwrap().satisfied);
        assert!(step(&p, "daemon").unwrap().satisfied);
        assert!(step(&p, "network").unwrap().satisfied);
        assert!(step(&p, "group").unwrap().satisfied);
        // The IOMMU is still off, and that is still correctly pending.
        assert!(!step(&p, "iommu").unwrap().satisfied);
    }

    /// The IOMMU step must name the right parameter for the host's CPU.
    #[test]
    fn the_iommu_step_names_the_cpus_own_parameter() {
        let p = plan(&work_hub());
        assert!(step(&p, "iommu").unwrap().detail.contains("amd_iommu=on"));

        let mut intel = work_hub();
        intel.cpu_vendor = CpuVendor::Intel;
        let p = plan(&intel);
        assert!(step(&p, "iommu").unwrap().detail.contains("intel_iommu=on"));
    }

    /// A bootloader nothing here can edit is blocked with an explanation,
    /// rather than offered a command that silently edits the wrong file.
    #[test]
    fn an_unknown_bootloader_blocks_the_iommu_step() {
        let mut f = work_hub();
        f.bootloader = Bootloader::Unknown;
        let p = plan(&f);
        let s = step(&p, "iommu").unwrap();
        assert!(s.command.is_none());
        assert!(s.blocked.as_deref().unwrap().contains("bootloader"));
    }
}
