# Spaces

Run whole machines on a Leo hub. Install the hypervisor, create Windows and
Linux guests, and hand one of them a GPU outright.

A Leo **dynamic app package**: the hub builds this binary from git and runs it
as a proxied subprocess. Leo itself is never recompiled.

```
Leo (Mac / iOS)  ──/p/spaces/──►  leo-spaces  ──►  libvirt / KVM  ──►  guests
     native UI                     (Rust)          qemu, OVMF, swtpm
                                        │
                                        └──► leo-spaces-admin  (the only root)
```

## What it is for

One box, several machines, and an LLM that can drive them. The motivating case
is Windows-only engineering software — SOLIDWORKS, simulation tools — running
on Linux metal, with agents working in parallel on it.

**Spaces** are the guests. **Host setup** is the hypervisor underneath them,
derived fresh on every load so a step that became true disappears. **Hardware**
is the graphics devices and whether each can be isolated for a guest.

## Install

From the Leo store, or by hand:

```bash
leo packages install descriptor.json \
  --mcp-command "/var/lib/leo/apps/spaces/src/target/release/leo-spaces mcp" \
  --enabled
```

Leave the **Host** setting blank to virtualise on the hub itself. Set it to a
hostname to drive a separate Leo compute machine over SSH.

Then, once, as root on that box:

```bash
sudo bash scripts/install-admin-wrapper.sh
```

That is the one step this package cannot take for itself, and the next section
is why.

## Root, and why there is a wrapper

Hypervisor setup is root work: installing packages, enabling units, writing
`/etc/modprobe.d`, editing the kernel command line. A Leo hub's service account
deliberately has **no blanket sudo** — giving it one would make every bug in
this package a root bug.

So the privileged surface is one root-owned program with a **closed verb set**:

```
leo-spaces-admin  version install-packages enable-daemon setup-network
                  setup-storage add-group fetch-virtio enable-iommu
                  bind-vfio <vendor:device> unbind-vfio
```

and one sudoers rule naming it. The grant is narrow because the program takes
*verbs*, not commands — the only caller-supplied value any verb accepts is a
PCI `vendor:device` pair, matched against a regex before it is written
anywhere. The installer verifies on the way out that the account it granted
cannot rewrite the program it was granted.

**Without the wrapper the package still works**, read-only: it reports what the
host is, derives the full plan, and shows the exact command for each step to
copy. It simply does not claim it can apply them.

Undo with `sudo bash scripts/install-admin-wrapper.sh --remove`.

## Setting a host up

Every step is derived from what the box actually is, and carries its own
command whether or not this process can run it.

| Step | What it does |
|---|---|
| `packages` | QEMU, libvirt, OVMF, swtpm, dnsmasq. Pacman, apt and dnf. |
| `daemon` | Enables libvirt — the monolithic socket, or the modular daemons. |
| `network` | Defines and autostarts the `default` NAT network. |
| `storage` | `/var/lib/leo-spaces` and the `leo-spaces` libvirt pool. |
| `group` | Puts the service account in `libvirt`. |
| `virtio` | Downloads the signed virtio-win driver ISO. |
| `iommu` | **Reboot.** Adds `amd_iommu=on iommu=pt` (or the Intel form). |
| `vfio:<addr>` | **Reboot.** Binds one card to vfio-pci so a guest can own it. |

The last two change how the box boots and are never bundled into a "set
everything up" action. The wrapper writes a timestamped backup first and
leaves any entry named `fallback`, `recovery` or `rescue` untouched, so there
is always a way back — and systemd-boot lets you edit the command line at the
menu with `e` regardless.

## Windows guests

Three things get set up without being asked, because all three are what a
Windows install needs and the failure when each is missing looks like something
else entirely:

- **UEFI firmware (OVMF).** Windows 11 will not install on SeaBIOS.
- **A software TPM 2.0 (swtpm).** Windows 11 setup refuses the disk without one.
- **The virtio driver ISO**, mounted as a second CD-ROM. Without it Windows
  setup reaches the disk-selection screen and lists **nothing**, because it
  cannot see a virtio disk. This is the most common way a Windows guest fails
  for a reason that reads as broken storage.

The default guest is **Windows Server 2022**, not Windows 11, and the reason is
the whole multi-agent story: Server licenses more than one concurrent
interactive session. Each session has its own desktop, its own input queue and
its own foreground window, which is what lets several agents drive one guest
without fighting — Windows has exactly one foreground window per session, and
a modal dialog grabs it.

`scripts/guest-agent/prepare-windows-guest.ps1` configures that side: enables
Remote Desktop, turns off one-session-per-user, keeps disconnected sessions
running, enables the hardware adapter for all sessions, and creates the agent
accounts. Run it inside the guest, elevated.

## GPU passthrough

A card is offered only when it can actually be handed over. Three things stop
it, and each is reported in words before anything reboots:

- **The IOMMU is off** — no device can be isolated at all.
- **The group is dirty** — VFIO claims an IOMMU *group*, not a device. A GPU
  grouped with the host's SATA controller takes the host's disks with it.
- **It is the host's own display** — passing it through leaves the console dark.

Integrated graphics are never offered. A passed-through card belongs to exactly
one space; assigning it twice is refused by name rather than discovered at boot
as a VFIO error that never mentions the other guest.

## Tools

| Tool | Actions |
|---|---|
| `spaces` | `list` `get` `start` `shutdown` `restart` `force_off` `pause` `resume` `autostart_on` `autostart_off` `create` `delete` `assign_gpu` `release_gpu` `snapshot` `restore` |
| `spaces_host` | `status` `plan` `apply` `gpus` |

Two tools rather than one, because the hub can restrict a tool and "may look at
the machines" and "may change how the box boots" are different grants.

`delete` keeps the disk unless told otherwise — a 120 GB Windows install with
somebody's work on it is not deleted as a side effect of removing a VM
definition.

## The UI, and why the tools are the real surface

A descriptor, not a web bundle: `GET /leo/ui/descriptor` returns a tree the Mac
and iOS clients render natively. `GET /leo/ui/data` is everything live.

**It cannot run as a dynamic app package, and that is not a bug in it.** The hub
jails an app package's HTTP process with bubblewrap. Confirmed by running the
hub's own confinement argv:

```
$ bwrap <the hub's flags> -- ls /run /var /etc/libvirt
ls: cannot access '/run': No such file or directory
ls: cannot access '/var': No such file or directory
ls: cannot access '/etc/libvirt': No such file or directory
```

Only `/usr`, a short allowlist of `/etc` files, `/proc`, `/dev`, a tmpfs `/tmp`,
the install directory and the package's own data directory are mounted — plus a
private network namespace. `virsh` is visible under `/usr/bin` and the socket it
needs at `/run/libvirt/libvirt-sock` is not, so a jailed process can never reach
libvirtd. The jail is right; a hypervisor manager is simply not the kind of
thing it is built to contain.

**The MCP server is not jailed.** The hub launches it as a plain subprocess, so
`spaces` and `spaces_host` reach the host normally. That is the working control
surface today, and everything in this package is reachable through it.

The descriptor is kept, tested and correct, for the two ways it becomes
renderable: as a compiled in-tree package on a hub with a checkout, or if the
hub ever grows an entitlement for an app that manages its own host.

Every mistake in a descriptor is silent — an unknown `kind` renders nothing, an
unknown `style` falls back to body text, an unseeded `stateKey` reads as `""`
and anything gated on it hides forever. Nothing logs. So the grammar is
checked in `cargo test`: every kind, prop, text style, colour token, action
shape, state key and REST path in the tree is verified against the vocabulary
the renderers implement, and against the routes `api::router()` actually
serves.

## Build and test

```bash
cargo build --release     # → target/release/leo-spaces
cargo test                # 79 tests, no hypervisor needed
```

The tests run without libvirt, without root and without a guest. They cover the
provisioning plan against synthetic hosts, the `virsh` output parse against
real `dominfo` text, the refusals (overcommit, double-assigned GPU, shell
metacharacters in a name), the MCP schema-to-dispatch agreement, and the
descriptor grammar.

One of them runs the host probe against the real machine, because the bug it
caught could not be caught any other way: `grep -qmE1` is a *usage* error —
grep reads `E1` as the argument to `-m` and exits 2 — and the `||` branch
recorded that as "this CPU cannot virtualise". Every host alive would have been
reported as unable to run a guest. The string is syntactically fine and a
mocked shell would have accepted it.

## `storage_mb` is sized for a build tree, not for data

The hub measures `entitlements.storage_mb` against `{data_dir}/apps/<name>`,
and for an app package that directory is the **whole checkout** — Rust
`target/` included, which is ~230 MB after a release build. Over 2× budget the
hub stops the process, so a small figure reads as a runaway and the app never
starts: no HTTP server, no `/leo/ui/descriptor`, and therefore no page in the
nav, with the reason only in the hub's journal.

This package keeps no data of its own — there is no database, libvirt holds the
domains, and guest disks live in `/var/lib/leo-spaces` — so the budget exists
purely as a ceiling on the build tree. 4096 covers it with headroom and still
guards against something genuinely running away.

## Limits worth knowing

- **Licences are the real ceiling.** Every concurrent instance of a
  seat-consuming application is a seat, and SOLIDWORKS needs a network licence
  (SNL) — standalone seats do not activate on a session host. RDS CALs are
  needed on top of the Windows Server licence. Both bind well before RAM
  or the GPU does.
- **OpenGL over RDS** is the flakiest layer in the stack, and CAD leans on it.
  Pilot before promising anyone a seat.
- **One GPU, one guest.** Passthrough is exclusive. Workstation cards have no
  vGPU support, so several GPU-backed guests means several cards. Parallelism
  *within* one guest, via sessions, is not subject to this — and most solver
  work is CPU-bound anyway.
- **No native page yet**, for the sandbox reason above — the tools are the
  surface. The descriptor is written, grammar-checked and ready for whichever
  of the two homes it ends up in.
- **No console yet.** Guests get a VNC display on the host's loopback (VNC
  rather than SPICE because guacd cannot bridge SPICE). Wiring that to an
  authenticated tunnel is the next piece.
- **No in-guest agent yet.** `prepare-windows-guest.ps1` builds the capacity
  for one; what runs inside the session is not in this package.
