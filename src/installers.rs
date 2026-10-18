//! The Installers panel's rows: the catalog, read against what the host has.
//!
//! Five states, because a download is the one thing on this surface that
//! takes minutes and can fail quietly in the background: ready, downloading
//! (with how far), failed (with the helper's last line), absent-but-fetchable,
//! and absent-and-brought-by-hand (Windows).

use serde::Serialize;

use crate::catalog::{self, Os};
use crate::facts::{Facts, IsoState};

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Standing {
    Ready,
    Downloading,
    Failed,
    /// Not on the host; the helper can fetch it.
    Fetchable,
    /// Not on the host; licensed, so somebody puts it there.
    Bring,
    /// In the import directory, waiting for root to adopt it.
    Staged,
}

#[derive(Debug, Clone, Serialize)]
pub struct Installer {
    pub id: &'static str,
    pub label: &'static str,
    pub family: &'static str,
    pub file: &'static str,
    pub path: String,
    pub standing: Standing,
    pub size_mb: u64,
    /// Why, when `standing` is `Failed`.
    pub failure: Option<String>,
    pub multi_session: bool,
}

pub fn of(os: &Os, f: &Facts) -> Installer {
    let have = f.iso(os.iso_file);
    let staged = f.staged.iter().find(|(id, _)| id == os.id);
    let standing = match have.map(|i| i.state) {
        Some(IsoState::Ready) => Standing::Ready,
        Some(IsoState::Downloading) => Standing::Downloading,
        Some(IsoState::Failed) => Standing::Failed,
        None if staged.is_some() => Standing::Staged,
        None if os.source.is_some() => Standing::Fetchable,
        None => Standing::Bring,
    };
    Installer {
        id: os.id,
        label: os.label,
        family: os.family.key(),
        file: os.iso_file,
        path: os.iso_path(),
        standing,
        size_mb: have.map(|i| i.size_mb).or(staged.map(|s| s.1)).unwrap_or(0),
        failure: have.and_then(|i| i.failure.clone()),
        multi_session: os.multi_session(),
    }
}

pub fn list(f: &Facts) -> Vec<Installer> {
    catalog::ALL.iter().map(|o| of(o, f)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts::IsoFile;
    use crate::host::Host;

    fn host_with(isos: Vec<IsoFile>) -> Facts {
        let mut f = crate::facts::probe_blank(&Host::Local);
        f.reachable = true;
        f.isos = isos;
        f
    }

    #[test]
    fn every_standing_is_reachable_from_what_the_host_has() {
        let f = host_with(vec![
            IsoFile {
                file: "archlinux-x86_64.iso".into(),
                size_mb: 1200,
                state: IsoState::Ready,
                failure: None,
            },
            IsoFile {
                file: "debian-13-amd64-netinst.iso".into(),
                size_mb: 300,
                state: IsoState::Downloading,
                failure: None,
            },
            IsoFile {
                file: "fedora-server-43-x86_64.iso".into(),
                size_mb: 0,
                state: IsoState::Failed,
                failure: Some("download of … failed".into()),
            },
        ]);
        let by = |id: &str| list(&f).into_iter().find(|i| i.id == id).unwrap();
        assert_eq!(by("archlinux").standing, Standing::Ready);
        assert_eq!(by("debian13").standing, Standing::Downloading);
        assert_eq!(by("debian13").size_mb, 300, "progress is the size so far");
        assert_eq!(by("fedora43").standing, Standing::Failed);
        assert!(by("fedora43").failure.is_some(), "a failure carries why");
        assert_eq!(by("rocky10").standing, Standing::Fetchable);
        assert_eq!(
            by("win2k22").standing,
            Standing::Fetchable,
            "Server evaluation media"
        );
        assert_eq!(by("win11").standing, Standing::Bring);
        let mut g = host_with(vec![]);
        g.staged = vec![("win11".into(), 9000)];
        let w = list(&g).into_iter().find(|i| i.id == "win11").unwrap();
        assert_eq!(w.standing, Standing::Staged);
        assert_eq!(w.size_mb, 9000);

        assert!(f.has_iso("archlinux-x86_64.iso"));
        assert!(
            !f.has_iso("debian-13-amd64-netinst.iso"),
            "a part is not an installer"
        );
    }
}
