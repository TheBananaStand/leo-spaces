//! The operating systems a space can be made from.
//!
//! One table, read by three things that must agree: the New-space sheet
//! (which OS ids exist and what to call them), `spaces::create` (which
//! installer file an id expects on the host), and the Installers panel
//! (which of those can be downloaded, and from where). The download side
//! lives in the root-owned helper, because `/var/lib/leo-spaces/iso` is
//! root's and an arbitrary URL must never be handed to root — the helper has
//! its own copy of the addresses, keyed by the same ids, and
//! `the_helpers_catalog_is_this_one` holds the two copies equal.
//!
//! The ids are libosinfo short ids, because that is what `virt-install
//! --osinfo` takes and what the guest's title records. Every one of them was
//! checked against `osinfo-query os` on a real host before it went in.

use crate::provision::STATE_DIR;

/// Which column of the picker an OS sits in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    Windows,
    Linux,
    Bsd,
}

impl Family {
    pub fn key(self) -> &'static str {
        match self {
            Self::Windows => "windows",
            Self::Linux => "linux",
            Self::Bsd => "bsd",
        }
    }

    /// The family as the sheet's segmented control spells it.
    pub fn label(self) -> &'static str {
        match self {
            Self::Windows => "Windows",
            Self::Linux => "Linux",
            Self::Bsd => "BSD",
        }
    }

    /// The reverse of [`key`](Self::key), for a family a request names.
    pub fn from_key(key: &str) -> Option<Self> {
        [Self::Windows, Self::Linux, Self::Bsd]
            .into_iter()
            .find(|f| f.key() == key)
    }
}

/// Where the helper can get the installer, when it can.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// A fixed address that stays current — a distribution's own `latest`
    /// or `Current` name.
    Url(&'static str),
    /// A directory listing and a pattern for the file in it, for the
    /// distributions whose file names carry a point release. The helper
    /// fetches the listing, takes the highest version that matches, and
    /// says so if nothing does.
    Dir {
        dir: &'static str,
        pattern: &'static str,
    },
}

#[derive(Debug, Clone, Copy)]
pub struct Os {
    pub id: &'static str,
    pub label: &'static str,
    pub family: Family,
    /// The file name under `/var/lib/leo-spaces/iso` the host is expected to
    /// have before a space of this OS can be defined.
    pub iso_file: &'static str,
    /// `None` when the person brings the image. Client Windows: Microsoft's
    /// ISO page issues a session link that dies in a day, and the Enterprise
    /// evaluation media it does publish a stable link for will not take a
    /// Pro or Home key. Windows Server has stable links to its evaluation
    /// media, which a key converts after install (`DISM /Set-Edition`), so
    /// Server is fetched.
    pub source: Option<Source>,
}

impl Os {
    pub fn iso_path(&self) -> String {
        format!("{STATE_DIR}/iso/{}", self.iso_file)
    }

    /// Windows Server licenses more than one interactive session at a time,
    /// which is the whole reason it is the default: several agents can each
    /// drive their own desktop on one guest.
    pub fn multi_session(&self) -> bool {
        self.id.starts_with("win2k")
    }
}

pub const ALL: &[Os] = &[
    // ── Windows ────────────────────────────────────────────────────────────
    // Microsoft's Evaluation Center links: each `fwlink` is the stable name
    // for the current en-US evaluation ISO, and the redirect behind it moves
    // when the media is refreshed. Checked 2026-10-17: 2025 → 26100.1742
    // (6.0 GB), 2022 → 20348 SERVER_EVAL (5.0 GB), 2019 → 17763.3650 (5.7 GB).
    Os { id: "win2k25", label: "Windows Server 2025", family: Family::Windows, iso_file: "win2k25.iso",
         source: Some(Source::Url("https://go.microsoft.com/fwlink/?linkid=2293312")) },
    Os { id: "win2k22", label: "Windows Server 2022", family: Family::Windows, iso_file: "win2k22.iso",
         source: Some(Source::Url("https://go.microsoft.com/fwlink/p/?linkid=2195280")) },
    Os { id: "win2k19", label: "Windows Server 2019", family: Family::Windows, iso_file: "win2k19.iso",
         source: Some(Source::Url("https://go.microsoft.com/fwlink/p/?linkid=2195167")) },
    Os { id: "win11", label: "Windows 11", family: Family::Windows, iso_file: "win11.iso", source: None },
    Os { id: "win10", label: "Windows 10", family: Family::Windows, iso_file: "win10.iso", source: None },
    // ── Linux ──────────────────────────────────────────────────────────────
    Os {
        id: "ubuntu24.04", label: "Ubuntu 24.04 LTS Server", family: Family::Linux,
        iso_file: "ubuntu-24.04-live-server-amd64.iso",
        source: Some(Source::Dir { dir: "https://releases.ubuntu.com/24.04/", pattern: "ubuntu-24\\.04(\\.[0-9]+)?-live-server-amd64\\.iso" }),
    },
    Os {
        id: "ubuntu26.04", label: "Ubuntu 26.04 LTS Server", family: Family::Linux,
        iso_file: "ubuntu-26.04-live-server-amd64.iso",
        source: Some(Source::Dir { dir: "https://releases.ubuntu.com/26.04/", pattern: "ubuntu-26\\.04(\\.[0-9]+)?-live-server-amd64\\.iso" }),
    },
    Os {
        id: "debian13", label: "Debian 13", family: Family::Linux,
        iso_file: "debian-13-amd64-netinst.iso",
        source: Some(Source::Dir { dir: "https://cdimage.debian.org/debian-cd/current/amd64/iso-cd/", pattern: "debian-13[0-9.]*-amd64-netinst\\.iso" }),
    },
    Os {
        id: "fedora43", label: "Fedora Server 43", family: Family::Linux,
        iso_file: "fedora-server-43-x86_64.iso",
        source: Some(Source::Dir { dir: "https://download.fedoraproject.org/pub/fedora/linux/releases/43/Server/x86_64/iso/", pattern: "Fedora-Server-dvd-x86_64-43-[0-9.]+\\.iso" }),
    },
    Os {
        id: "archlinux", label: "Arch Linux", family: Family::Linux,
        iso_file: "archlinux-x86_64.iso",
        source: Some(Source::Url("https://geo.mirror.pkgbuild.com/iso/latest/archlinux-x86_64.iso")),
    },
    Os {
        id: "rocky10", label: "Rocky Linux 10", family: Family::Linux,
        iso_file: "rocky-10-x86_64-minimal.iso",
        source: Some(Source::Url("https://download.rockylinux.org/pub/rocky/10/isos/x86_64/Rocky-10-latest-x86_64-minimal.iso")),
    },
    Os {
        id: "rocky9", label: "Rocky Linux 9", family: Family::Linux,
        iso_file: "rocky-9-x86_64-minimal.iso",
        source: Some(Source::Url("https://download.rockylinux.org/pub/rocky/9/isos/x86_64/Rocky-9-latest-x86_64-minimal.iso")),
    },
    Os {
        id: "almalinux10", label: "AlmaLinux 10", family: Family::Linux,
        iso_file: "almalinux-10-x86_64-minimal.iso",
        source: Some(Source::Url("https://repo.almalinux.org/almalinux/10/isos/x86_64/AlmaLinux-10-latest-x86_64-minimal.iso")),
    },
    Os {
        id: "opensuse16.0", label: "openSUSE Leap 16.0", family: Family::Linux,
        iso_file: "opensuse-leap-16.0-x86_64.iso",
        source: Some(Source::Dir { dir: "https://download.opensuse.org/distribution/leap/16.0/iso/", pattern: "[A-Za-z-]*Leap-16\\.0[^\"<> ]*x86_64[^\"<> ]*\\.iso" }),
    },
    Os {
        id: "opensusetumbleweed", label: "openSUSE Tumbleweed", family: Family::Linux,
        iso_file: "opensuse-tumbleweed-x86_64.iso",
        source: Some(Source::Url("https://download.opensuse.org/tumbleweed/iso/openSUSE-Tumbleweed-DVD-x86_64-Current.iso")),
    },
    // ── BSD ────────────────────────────────────────────────────────────────
    Os {
        id: "freebsd15.0", label: "FreeBSD 15.0", family: Family::Bsd,
        iso_file: "freebsd-15.0-amd64-disc1.iso",
        source: Some(Source::Url("https://download.freebsd.org/releases/amd64/amd64/ISO-IMAGES/15.0/FreeBSD-15.0-RELEASE-amd64-disc1.iso")),
    },
    Os {
        id: "freebsd14.3", label: "FreeBSD 14.3", family: Family::Bsd,
        iso_file: "freebsd-14.3-amd64-disc1.iso",
        source: Some(Source::Url("https://download.freebsd.org/releases/amd64/amd64/ISO-IMAGES/14.3/FreeBSD-14.3-RELEASE-amd64-disc1.iso")),
    },
];

pub fn find(id: &str) -> Option<&'static Os> {
    ALL.iter().find(|o| o.id == id)
}

/// The ids, for a refusal that lists what would have worked.
pub fn ids() -> Vec<&'static str> {
    ALL.iter().map(|o| o.id).collect()
}

/// The helper's copy of this table, one line per downloadable OS:
/// `id|file|url|<address>` or `id|file|dir|<directory>|<pattern>`.
///
/// Rendered from here so the two copies can be compared byte for byte, and
/// so adding an OS is one edit rather than two.
#[cfg(test)]
pub fn helper_table() -> String {
    let mut out = String::new();
    for o in ALL {
        match o.source {
            Some(Source::Url(u)) => out.push_str(&format!("{}|{}|url|{u}\n", o.id, o.iso_file)),
            Some(Source::Dir { dir, pattern }) => {
                out.push_str(&format!("{}|{}|dir|{dir}|{pattern}\n", o.id, o.iso_file))
            }
            None => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_and_files_are_unique() {
        let mut ids: Vec<&str> = ALL.iter().map(|o| o.id).collect();
        let mut files: Vec<&str> = ALL.iter().map(|o| o.iso_file).collect();
        ids.sort();
        files.sort();
        ids.dedup();
        files.dedup();
        assert_eq!(ids.len(), ALL.len(), "two entries share an id");
        assert_eq!(
            files.len(),
            ALL.len(),
            "two entries would download onto each other"
        );
    }

    /// An id reaches `virt-install --osinfo` and a file name reaches a shell
    /// line, so both are held to what those can take without quoting.
    #[test]
    fn ids_and_files_are_plain() {
        for o in ALL {
            assert!(
                o.id.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.'),
                "{} is not a libosinfo short id",
                o.id
            );
            assert!(
                o.iso_file.ends_with(".iso")
                    && o.iso_file
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_')),
                "{} is not a plain file name",
                o.iso_file
            );
        }
    }

    /// Client Windows is brought; everything else — Windows Server's
    /// evaluation media included — is fetched. See `Os::source`.
    #[test]
    fn client_windows_is_brought_and_everything_else_is_fetched() {
        for o in ALL {
            if o.family == Family::Windows && !o.multi_session() {
                assert!(
                    o.source.is_none(),
                    "{} has no stable address to fetch from",
                    o.id
                );
            } else {
                assert!(o.source.is_some(), "{} has no way onto the host", o.id);
            }
        }
    }

    /// Every address is https and every directory ends in a slash, because
    /// the helper joins `dir` and the matched name with nothing between them.
    #[test]
    fn sources_are_well_formed() {
        for o in ALL {
            match o.source {
                Some(Source::Url(u)) => assert!(
                    u.starts_with("https://")
                        && (u.ends_with(".iso") || u.contains("go.microsoft.com/fwlink")),
                    "{u}"
                ),
                Some(Source::Dir { dir, pattern }) => {
                    assert!(dir.starts_with("https://") && dir.ends_with('/'), "{dir}");
                    assert!(
                        pattern.ends_with("\\.iso"),
                        "{pattern} would match a checksum file too"
                    );
                    assert!(
                        !pattern.contains('|'),
                        "a pipe would break the helper's table"
                    );
                }
                None => {}
            }
        }
    }

    /// The table the helper carries IS this one. The helper is a root-owned
    /// program with its own copy of the addresses (nothing the app sends it
    /// may be an address), and two copies that drift would download one
    /// thing and expect another.
    #[test]
    fn the_helpers_catalog_is_this_one() {
        let script = include_str!("../scripts/leo-spaces-admin");
        let start = script
            .find("ISO_CATALOG='\n")
            .expect("the helper declares ISO_CATALOG")
            + "ISO_CATALOG='\n".len();
        let end = script[start..].find("'\n").expect("…and closes it") + start;
        assert_eq!(
            &script[start..end],
            helper_table(),
            "scripts/leo-spaces-admin's ISO_CATALOG has drifted from catalog.rs — paste `helper_table()` in"
        );
    }

    #[test]
    fn the_default_os_is_in_the_catalog_and_is_multi_session() {
        let d = find("win2k22").expect("the default");
        assert!(d.multi_session());
        assert!(!find("win11").unwrap().multi_session());
        assert!(find("nope").is_none());
    }
}
