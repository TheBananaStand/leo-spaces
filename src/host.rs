//! The one place a command reaches the machine that runs the guests.
//!
//! A space host is either **this** box — a Leo hub virtualising on its own
//! metal — or a Leo compute machine reached over SSH. Everything above this
//! module (provisioning, the libvirt driver, the guest bridge) is written
//! against `Host::run` and does not know which it got.
//!
//! Why one funnel rather than two code paths: a provisioning step is a shell
//! command with an exit code, and the difference between running it here and
//! running it there is argv assembly. Two paths means every new step is
//! written twice and tested once.

use std::fmt;
use std::time::Duration;

use tokio::process::Command;

/// How long any single host command may take before it is abandoned.
///
/// Package installs are the slow case — a cold `pacman -S qemu-full` pulls
/// several hundred megabytes — so this is generous. It exists at all because
/// a hung `ssh` to a box that stopped answering would otherwise pin a request
/// handler until the hub restarted.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(900);

/// Where the guests run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Host {
    /// This machine. The hub is the hypervisor.
    Local,
    /// A Leo compute machine, reached over SSH with a key.
    Remote {
        host: String,
        user: String,
        port: u16,
        key_path: Option<String>,
    },
}

impl fmt::Display for Host {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Local => write!(f, "local"),
            Self::Remote { host, user, .. } => write!(f, "{user}@{host}"),
        }
    }
}

/// What a command did. Non-zero exits are *data*, not errors: a provisioning
/// probe like `systemctl is-active libvirtd` answers by failing, and a driver
/// that treated that as an error would have to catch it back out again.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Output {
    pub status: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Output {
    pub fn ok(&self) -> bool {
        self.status == 0
    }

    /// stdout with trailing newline removed — what nearly every caller wants.
    pub fn trimmed(&self) -> &str {
        self.stdout.trim_end_matches(['\n', '\r'])
    }

    /// The best available description of a failure, for an error message.
    ///
    /// stderr first because that is where tools explain themselves; stdout as
    /// the fallback because some write their complaint there; and a mention of
    /// the exit code when both are empty, because "it failed" with no text at
    /// all is the one report that cannot be acted on.
    pub fn failure(&self) -> String {
        let err = self.stderr.trim();
        if !err.is_empty() {
            return err.to_string();
        }
        let out = self.stdout.trim();
        if !out.is_empty() {
            return out.to_string();
        }
        format!("exited {}", self.status)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum HostError {
    #[error("could not run `{command}` on {host}: {source}")]
    Spawn {
        host: String,
        command: String,
        source: std::io::Error,
    },
    #[error("`{command}` on {host} did not finish within {secs}s")]
    Timeout {
        host: String,
        command: String,
        secs: u64,
    },
}

impl Host {
    /// Read the host out of the settings the hub injected.
    ///
    /// An empty or absent `spaces_host` means this box, which is the common
    /// case and the one that must need no configuration: installing the
    /// package on the hub you want to virtualise on should be the whole setup.
    pub fn from_settings(
        host: Option<String>,
        user: Option<String>,
        port: Option<u16>,
        key_path: Option<String>,
    ) -> Self {
        match host.map(|h| h.trim().to_string()).filter(|h| !h.is_empty()) {
            None => Self::Local,
            Some(h) if h == "local" || h == "localhost" || h == "127.0.0.1" => Self::Local,
            Some(h) => Self::Remote {
                host: h,
                user: user.filter(|u| !u.is_empty()).unwrap_or_else(|| "root".into()),
                port: port.unwrap_or(22),
                key_path: key_path.filter(|k| !k.is_empty()),
            },
        }
    }

    pub fn is_local(&self) -> bool {
        matches!(self, Self::Local)
    }

    /// Run a shell command on the host and collect its output.
    ///
    /// The command is handed to `bash -lc` in both modes, so a caller may use
    /// pipes and globs and get the same parsing in either. That is a
    /// deliberate trade: these commands are composed in this crate from
    /// validated parts, never from user text, and writing provisioning steps
    /// as argv vectors made them unreadable without making them safer.
    pub async fn run(&self, command: &str) -> Result<Output, HostError> {
        self.run_with_timeout(command, DEFAULT_TIMEOUT).await
    }

    pub async fn run_with_timeout(
        &self,
        command: &str,
        timeout: Duration,
    ) -> Result<Output, HostError> {
        let mut cmd = self.command_for(command);
        // Never inherit a tty: a provisioning step that stops to ask something
        // would hang until the timeout instead of failing with its question.
        cmd.stdin(std::process::Stdio::null());

        let fut = cmd.output();
        let out = match tokio::time::timeout(timeout, fut).await {
            Err(_) => {
                return Err(HostError::Timeout {
                    host: self.to_string(),
                    command: command.to_string(),
                    secs: timeout.as_secs(),
                })
            }
            Ok(Err(source)) => {
                return Err(HostError::Spawn {
                    host: self.to_string(),
                    command: command.to_string(),
                    source,
                })
            }
            Ok(Ok(out)) => out,
        };

        Ok(Output {
            // A process killed by a signal has no exit code. Reporting that as
            // 0 would read as success, so it becomes -1: not a real exit
            // status, and not mistakable for one.
            status: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }

    fn command_for(&self, command: &str) -> Command {
        match self {
            Self::Local => {
                let mut c = Command::new("bash");
                c.arg("-lc").arg(command);
                c
            }
            Self::Remote {
                host,
                user,
                port,
                key_path,
            } => {
                let mut c = Command::new("ssh");
                c.arg("-o").arg("BatchMode=yes")
                    .arg("-o").arg("StrictHostKeyChecking=accept-new")
                    .arg("-o").arg("ConnectTimeout=10")
                    .arg("-p").arg(port.to_string());
                if let Some(key) = key_path {
                    c.arg("-i").arg(key);
                }
                c.arg(format!("{user}@{host}"));
                // The remote shell gets the command as a single argument, so
                // the local shell never sees it and cannot expand it twice.
                c.arg("bash").arg("-lc").arg(command);
                c
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An absent host setting must mean *this box*, not a misconfiguration.
    ///
    /// The whole one-click story depends on it: install the package on the hub
    /// and it virtualises there with nothing filled in. A default of "" that
    /// became `Remote { host: "" }` would turn that into an ssh to nowhere.
    #[test]
    fn no_host_setting_means_local() {
        assert_eq!(Host::from_settings(None, None, None, None), Host::Local);
        assert_eq!(
            Host::from_settings(Some("".into()), None, None, None),
            Host::Local
        );
        assert_eq!(
            Host::from_settings(Some("   ".into()), None, None, None),
            Host::Local
        );
        // Spelled-out loopback is still this box. Going through ssh to reach
        // 127.0.0.1 needs sshd running and a key installed to talk to itself,
        // which is a failure the owner cannot be expected to diagnose from
        // "connection refused".
        assert_eq!(
            Host::from_settings(Some("localhost".into()), None, None, None),
            Host::Local
        );
        assert_eq!(
            Host::from_settings(Some("127.0.0.1".into()), None, None, None),
            Host::Local
        );
    }

    #[test]
    fn a_named_host_becomes_remote_with_defaults() {
        let h = Host::from_settings(Some("compute-1.lan".into()), None, None, None);
        assert_eq!(
            h,
            Host::Remote {
                host: "compute-1.lan".into(),
                user: "root".into(),
                port: 22,
                key_path: None,
            }
        );
        assert!(!h.is_local());
    }

    /// Non-zero exit is data. `systemctl is-active` answers "no" by exiting 3,
    /// and the provisioning probes read that as a fact about the host rather
    /// than as a transport failure.
    #[tokio::test]
    async fn a_failing_command_is_output_not_error() {
        let out = Host::Local.run("exit 3").await.expect("spawn should succeed");
        assert_eq!(out.status, 3);
        assert!(!out.ok());
    }

    #[tokio::test]
    async fn stdout_comes_back_trimmed() {
        let out = Host::Local.run("echo hello").await.unwrap();
        assert!(out.ok());
        assert_eq!(out.trimmed(), "hello");
    }

    /// A failure with output on neither stream still has to say something an
    /// operator can act on, which is why `failure()` falls back to the code.
    #[test]
    fn a_silent_failure_still_reports_its_code() {
        let out = Output { status: 7, stdout: "".into(), stderr: "".into() };
        assert_eq!(out.failure(), "exited 7");

        let out = Output { status: 1, stdout: "said here".into(), stderr: "".into() };
        assert_eq!(out.failure(), "said here");

        let out = Output { status: 1, stdout: "ignored".into(), stderr: "the reason".into() };
        assert_eq!(out.failure(), "the reason");
    }

    /// A timeout must name the host and the command. The failure it replaced
    /// was a request handler pinned forever on an ssh to a box that stopped
    /// answering, with nothing in the log to say which box or why.
    #[tokio::test]
    async fn a_hung_command_times_out_and_says_what_hung() {
        let err = Host::Local
            .run_with_timeout("sleep 5", Duration::from_millis(150))
            .await
            .expect_err("should time out");
        let msg = err.to_string();
        assert!(msg.contains("sleep 5"), "should name the command: {msg}");
        assert!(msg.contains("local"), "should name the host: {msg}");
    }
}
