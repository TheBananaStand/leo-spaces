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
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

use tokio::process::Command;

/// How long any single host command may take before it is abandoned.
///
/// Package installs are the slow case — a cold `pacman -S qemu-full` pulls
/// several hundred megabytes — so this is generous. It exists at all because
/// a hung `ssh` to a box that stopped answering would otherwise pin a request
/// handler until the hub restarted.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(900);

/// How long a *read* — the host probe, the guest inventory — may take.
///
/// Both answer in well under a second, and both sit on every page load, so
/// the only way either reaches a minute is a host that stopped answering
/// mid-connection. With one multiplexed ssh carrying every command, that is
/// exactly the case a per-command `ConnectTimeout` no longer catches: a
/// client attaches to a master whose peer has gone quiet and waits for the
/// keepalive to notice. Reads give up here instead of pinning a page for the
/// fifteen minutes a package install is allowed.
pub const READ_TIMEOUT: Duration = Duration::from_secs(60);

/// How long an idle multiplexed connection is kept open after its last
/// command, in seconds. A page load is two commands and an action is three
/// or four, a few seconds apart, with a person between the bursts.
const CONTROL_PERSIST_SECS: u32 = 120;

/// Where ssh keeps the sockets its multiplexed connections are shared
/// through — one per `user@host:port`, hashed by ssh itself (`%C`).
///
/// **Every remote command used to pay a full ssh handshake**: key exchange,
/// authentication and a login shell, ~115 ms each from the work hub to its
/// compute box, measured. A page load is two commands, so that was 230 ms of
/// the 670 before anything rendered; the Enter presses that answer a Windows
/// installer were drifting from the half-second cadence they claim to 615 ms.
/// With a control master the second command costs 5 ms.
///
/// The directory must be private (a socket anybody can connect to is a
/// shell on the host as this account) and the path short (a Unix socket
/// path is capped at 108 bytes). `$XDG_RUNTIME_DIR` is both by construction;
/// a box without one gets a 0700 directory under the temp dir, created here
/// — and if something else owns a path by that name, multiplexing is simply
/// not used rather than used through a directory this process did not make.
fn control_dir() -> Option<&'static PathBuf> {
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIR.get_or_init(|| {
        use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
        let base = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .filter(|p| p.is_dir())
            .unwrap_or_else(std::env::temp_dir);
        let me = std::fs::metadata("/proc/self").map(|m| m.uid()).ok()?;
        let dir = base.join(format!("leo-spaces-ssh-{me}"));
        match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => {
                tracing::warn!(
                    "cannot make {}: {e} — ssh connections are not shared",
                    dir.display()
                );
                return None;
            }
        }
        // Whoever made it, it has to be ours and closed, or it is not used.
        let meta = std::fs::symlink_metadata(&dir).ok()?;
        if !meta.is_dir() || meta.uid() != me || meta.permissions().mode() & 0o077 != 0 {
            tracing::warn!(
                "{} is not a private directory of this account — ssh connections are not shared",
                dir.display()
            );
            return None;
        }
        Some(dir)
    })
    .as_ref()
}

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

/// The two ends of [`Host::tcp_pipe`]: a socket, or an ssh whose stdio is one.
pub enum Pipe {
    Tcp(tokio::net::TcpStream),
    Ssh(tokio::process::Child),
}

impl Pipe {
    /// Split into something to read from and something to write to, so a
    /// caller can pump both directions at once.
    pub fn split(
        self,
    ) -> (
        Box<dyn tokio::io::AsyncRead + Unpin + Send>,
        Box<dyn tokio::io::AsyncWrite + Unpin + Send>,
        Option<tokio::process::Child>,
    ) {
        match self {
            Self::Tcp(stream) => {
                let (r, w) = stream.into_split();
                (Box::new(r), Box::new(w), None)
            }
            Self::Ssh(mut child) => {
                let stdout = child.stdout.take().expect("piped");
                let stdin = child.stdin.take().expect("piped");
                (Box::new(stdout), Box::new(stdin), Some(child))
            }
        }
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
                user: user
                    .filter(|u| !u.is_empty())
                    .unwrap_or_else(|| "root".into()),
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
    /// Put a local file at `remote` on the host, as this process's account
    /// there. A copy for a local host, `scp` with the same key for a remote
    /// one — no shell quoting of file contents anywhere.
    pub async fn upload(&self, local: &std::path::Path, remote: &str) -> Result<(), HostError> {
        let fail = |e: std::io::Error| HostError::Spawn {
            host: self.to_string(),
            command: format!("upload {}", local.display()),
            source: e,
        };
        match self {
            Self::Local => {
                tokio::fs::copy(local, remote).await.map_err(fail)?;
                Ok(())
            }
            Self::Remote {
                host,
                user,
                port,
                key_path,
            } => {
                let mut c = Command::new("scp");
                c.arg("-q")
                    .arg("-o")
                    .arg("BatchMode=yes")
                    .arg("-o")
                    .arg("StrictHostKeyChecking=accept-new")
                    .arg("-P")
                    .arg(port.to_string());
                if let Some(key) = key_path {
                    c.arg("-i").arg(key);
                }
                multiplex(&mut c);
                c.arg(local).arg(format!("{user}@{host}:{remote}"));
                c.stdin(std::process::Stdio::null());
                let out = c.output().await.map_err(fail)?;
                if !out.status.success() {
                    return Err(HostError::Spawn {
                        host: self.to_string(),
                        command: format!("scp {} → {remote}", local.display()),
                        source: std::io::Error::other(
                            String::from_utf8_lossy(&out.stderr).trim().to_string(),
                        ),
                    });
                }
                Ok(())
            }
        }
    }

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

    /// A byte pipe to a TCP port on the host's loopback — what a guest's VNC
    /// console is reached through.
    ///
    /// Remote hosts use `ssh -W`, which is ssh's own netcat: stdin and stdout
    /// become the far end's `127.0.0.1:<port>` with nothing installed over
    /// there and the same key the rest of this package already holds. A
    /// guest's graphics listen on loopback only (see `NewSpace::command`), so
    /// this is the one road to them, and it is authenticated end to end.
    pub async fn tcp_pipe(&self, port: u16) -> Result<Pipe, HostError> {
        match self {
            Self::Local => {
                let stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
                    .await
                    .map_err(|e| HostError::Spawn {
                        command: format!("connect 127.0.0.1:{port}"),
                        host: self.to_string(),
                        source: e,
                    })?;
                Ok(Pipe::Tcp(stream))
            }
            Self::Remote {
                host,
                user,
                port: ssh_port,
                key_path,
            } => {
                let mut c = ssh(*ssh_port, key_path.as_deref());
                c.arg("-W")
                    .arg(format!("127.0.0.1:{port}"))
                    .arg(format!("{user}@{host}"))
                    .stdin(std::process::Stdio::piped())
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::null())
                    .kill_on_drop(true);
                let child = c.spawn().map_err(|e| HostError::Spawn {
                    command: format!("ssh -W 127.0.0.1:{port}"),
                    host: self.to_string(),
                    source: e,
                })?;
                Ok(Pipe::Ssh(child))
            }
        }
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
                let mut c = ssh(*port, key_path.as_deref());
                c.arg(format!("{user}@{host}"));
                // **ssh does not preserve argv boundaries.** It joins every
                // remaining argument with a single space and hands the result
                // to the remote *login shell*, which parses it again. So
                // passing `bash`, `-lc`, `<command>` as three arguments does
                // NOT deliver the command as one: the far side receives
                //
                //     bash -lc sudo -n /usr/local/bin/leo-spaces-admin install-packages
                //
                // and runs the command string `sudo` with the rest as
                // positional parameters. Bare `sudo` prints its usage, which
                // is what every remote Apply returned — "packages failed:
                // usage: sudo -h | -K | -k | -V", a message about this bug
                // wearing the clothes of a privilege problem.
                //
                // Quoting it here is what makes the comment above true. The
                // remote shell unwraps one layer and `bash -lc` gets the whole
                // command as a single word, exactly as the local arm does.
                c.arg("bash").arg("-lc").arg(single_quote(command));
                c
            }
        }
    }
}

/// An `ssh` with the options every use here shares: non-interactive, a
/// bounded connect, keepalives so a host that goes quiet is noticed within
/// a minute rather than at the command's own timeout, and the connection
/// shared through [`control_dir`] when there is one.
fn ssh(port: u16, key_path: Option<&str>) -> Command {
    let mut c = Command::new("ssh");
    c.arg("-o")
        .arg("BatchMode=yes")
        .arg("-o")
        .arg("StrictHostKeyChecking=accept-new")
        .arg("-o")
        .arg("ConnectTimeout=10")
        .arg("-o")
        .arg("ServerAliveInterval=15")
        .arg("-o")
        .arg("ServerAliveCountMax=3")
        .arg("-p")
        .arg(port.to_string());
    if let Some(key) = key_path {
        c.arg("-i").arg(key);
    }
    multiplex(&mut c);
    c
}

/// Share the connection through a control master — `auto`, so the first
/// command opens it and every later one rides it, and a master that has
/// gone away is replaced rather than waited on.
fn multiplex(c: &mut Command) {
    let Some(dir) = control_dir() else { return };
    c.arg("-o")
        .arg("ControlMaster=auto")
        .arg("-o")
        .arg(format!("ControlPath={}/%C", dir.display()))
        .arg("-o")
        .arg(format!("ControlPersist={CONTROL_PERSIST_SECS}"));
}

/// Wrap `s` so a shell parses it back as one word.
///
/// POSIX single-quoting: everything inside `'...'` is literal, and the only
/// thing that cannot appear is a single quote, which is closed, escaped and
/// reopened (`'\''`). That covers every character a provisioning command can
/// carry — the `vendor:device` pairs, the paths, the spaces — without needing
/// to know which ones the far shell treats specially.
fn single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **ssh flattens argv, and this is the test that says so.**
    ///
    /// Every remote Apply failed with `usage: sudo -h | -K | -k | -V` — sudo's
    /// own usage, which reads like a privilege problem and is not one. ssh
    /// joins its trailing arguments with spaces and hands the string to the
    /// remote login shell, so `bash`, `-lc`, `sudo -n … install-packages` as
    /// three arguments arrives as `bash -lc sudo -n … install-packages`: the
    /// command string is `sudo` and the rest are positional parameters. Bare
    /// `sudo` prints usage.
    ///
    /// Asserted on the argv rather than by running anything, because the bug
    /// is entirely in how the command is handed over — it reproduces against
    /// any host and no test should need one.
    #[test]
    fn a_remote_command_reaches_the_far_shell_as_one_word() {
        let host = Host::Remote {
            host: "192.168.0.100".into(),
            user: "leo".into(),
            port: 22,
            key_path: Some("/home/leo/.ssh/id_ed25519".into()),
        };
        let cmd = host.command_for("sudo -n /usr/local/bin/leo-spaces-admin install-packages");
        let argv: Vec<String> = cmd
            .as_std()
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();

        let last = argv.last().expect("ssh is given a remote command");
        assert_eq!(
            last, "'sudo -n /usr/local/bin/leo-spaces-admin install-packages'",
            "the remote command must be quoted; unquoted, ssh flattens it and the far \
             shell runs `sudo` with the rest as positional parameters"
        );
        // And the shape around it: `bash -lc <quoted>` are the final three.
        let tail = &argv[argv.len() - 3..];
        assert_eq!(tail[0], "bash");
        assert_eq!(tail[1], "-lc");
    }

    /// Every remote command rides one shared connection.
    ///
    /// The handshake was ~115 ms per command, measured from the work hub to
    /// its compute box, and a page load is two commands with the installer's
    /// Enter presses twenty-two more. The argv is what carries the sharing,
    /// so the argv is what is checked — and the directory the sockets live
    /// in has to be this account's and closed to everyone else, because a
    /// reachable control socket is a shell on the host.
    #[test]
    fn a_remote_command_rides_a_shared_private_connection() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let host = Host::Remote {
            host: "192.168.0.100".into(),
            user: "leo".into(),
            port: 22,
            key_path: None,
        };
        let cmd = host.command_for("true");
        let argv: Vec<String> = cmd
            .as_std()
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let dir = control_dir().expect("a private socket directory can be made here");
        assert!(argv.iter().any(|a| a == "ControlMaster=auto"), "{argv:?}");
        assert!(
            argv.iter()
                .any(|a| *a == format!("ControlPath={}/%C", dir.display())),
            "{argv:?}"
        );
        assert!(
            argv.iter().any(|a| a.starts_with("ControlPersist=")),
            "{argv:?}"
        );
        // Keepalives, so a master whose peer went quiet is noticed in under a
        // minute instead of at the command's own timeout.
        assert!(
            argv.iter().any(|a| a == "ServerAliveInterval=15"),
            "{argv:?}"
        );

        let meta = std::fs::metadata(dir).unwrap();
        assert!(meta.is_dir());
        assert_eq!(
            meta.permissions().mode() & 0o777,
            0o700,
            "{}",
            dir.display()
        );
        assert_eq!(
            meta.uid(),
            std::fs::metadata("/proc/self").unwrap().uid(),
            "the socket directory belongs to this account"
        );
        // A socket path is capped at 108 bytes; ssh's %C is 40.
        assert!(dir.as_os_str().len() + 1 + 40 < 108, "{}", dir.display());

        // The console's byte pipe and the local arm are built the same way.
        let pipe = {
            let mut c = ssh(22, None);
            c.arg("-W").arg("127.0.0.1:5900");
            c
        };
        let pargv: Vec<String> = pipe
            .as_std()
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(pargv.iter().any(|a| a == "ControlMaster=auto"));
        let local = Host::Local.command_for("true");
        assert_eq!(local.as_std().get_program(), "bash");
    }

    /// A read gives up long before an install would.
    #[test]
    fn a_read_times_out_sooner_than_an_install() {
        assert!(READ_TIMEOUT < DEFAULT_TIMEOUT);
        assert!(
            READ_TIMEOUT >= Duration::from_secs(30),
            "a slow host still answers"
        );
    }

    /// A command carrying a single quote must not end the quoting early — the
    /// close/escape/reopen dance is the only part of POSIX single-quoting that
    /// is not trivial, so it gets its own case.
    #[test]
    fn a_quote_inside_the_command_survives() {
        assert_eq!(single_quote("echo hi"), "'echo hi'");
        assert_eq!(single_quote("it's"), r#"'it'\''s'"#);
        // Round-trip through a real shell: what comes back must be the input.
        let out = std::process::Command::new("bash")
            .arg("-c")
            .arg(format!("printf %s {}", single_quote("a 'b' c")))
            .output()
            .expect("bash runs");
        assert_eq!(String::from_utf8_lossy(&out.stdout), "a 'b' c");
    }

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
        let out = Host::Local
            .run("exit 3")
            .await
            .expect("spawn should succeed");
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
        let out = Output {
            status: 7,
            stdout: "".into(),
            stderr: "".into(),
        };
        assert_eq!(out.failure(), "exited 7");

        let out = Output {
            status: 1,
            stdout: "said here".into(),
            stderr: "".into(),
        };
        assert_eq!(out.failure(), "said here");

        let out = Output {
            status: 1,
            stdout: "ignored".into(),
            stderr: "the reason".into(),
        };
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
