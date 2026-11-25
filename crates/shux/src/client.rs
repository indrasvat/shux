//! Client-side daemon auto-start and connection logic.
//!
//! The CLI probes the UDS. On `ConnectionRefused` or `NotFound`, it spawns
//! the daemon via re-exec, then retries with exponential backoff (PRD 4.5).
//!
//! On successful connection, the client performs a version handshake. A
//! daemon of another build (e.g. after an upgrade) is replaced only when it
//! holds no sessions; one that holds any, or cannot say, is left running and
//! the command is refused with [`ClientError::DaemonInUse`].

use std::io;
use std::path::Path;
use std::time::Duration;

use serde_json::json;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tracing::{debug, info, warn};

use crate::daemon::{self, DaemonError};

/// The version of this binary, baked in at compile time.
const CLIENT_VERSION: &str = shux_rpc::VERSION;

/// The git SHA of this binary, baked in at compile time.
const CLIENT_GIT_SHA: &str = shux_rpc::GIT_SHA;

/// How long the client waits for the daemon it started to accept connections.
struct ConnectBudget {
    /// Probes before giving up on a daemon that is no longer starting.
    retries: u32,
    /// First backoff, doubled after each refused probe up to `max_backoff`.
    backoff: Duration,
    max_backoff: Duration,
    /// Past `retries`, a daemon still starting is waited for until this long
    /// after the spawn: a loaded machine can take longer than the backoff.
    deadline: Duration,
}

/// A daemon binds a few hundred milliseconds after it is spawned; a probe
/// that just misses it costs at most `max_backoff` more.
const CONNECT_BUDGET: ConnectBudget = ConnectBudget {
    retries: 20,
    backoff: Duration::from_millis(25),
    max_backoff: Duration::from_millis(100),
    deadline: Duration::from_secs(30),
};

/// How many stale daemons one client stops before giving up: each stop ends an
/// identified process, so only clients of another build racing to start their
/// own could keep producing more.
const MAX_STALE_STOPS: u32 = 8;

/// How long to wait for another client to finish starting the daemon: one
/// stale daemon's replacement (a probe and its exit) and the start.
fn start_lock_timeout() -> Duration {
    crate::patience::peer_patience() * 2 + CONNECT_BUDGET.deadline
}

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("daemon error")]
    Daemon(#[from] DaemonError),
    #[error("failed to connect to daemon after {0} retries")]
    ConnectionFailed(u32),
    #[error("I/O error")]
    Io(#[from] io::Error),
    #[error(
        "the running daemon (pid {0}) is from another shux build and did not exit after SIGTERM; \
         it may be draining a session -- retry, or run `shux daemon stop`"
    )]
    StaleDaemonAlive(u32),
    #[error(
        "a daemon for this socket{} is still running but has not served it for {:?}; \
         run `shux daemon stop` and retry",
        .0, crate::patience::peer_patience()
    )]
    DaemonNotServing(String),
    #[error(
        "the daemon for this socket{} started but did not serve it within {}s; \
         run `shux daemon stop` and retry",
        .0, CONNECT_BUDGET.deadline.as_secs()
    )]
    DaemonNotStarted(String),
    #[error(
        "the daemon serving {}{} did not answer the version check within {:?}; it may be \
         busy, stopped or wedged -- retry, and if it persists run `shux daemon stop`",
        .1.display(), .0, crate::patience::peer_patience()
    )]
    DaemonUnresponsive(String, std::path::PathBuf),
    #[error(
        "stopped {MAX_STALE_STOPS} daemons from another shux build on this socket and \
         another took over each time; stop the other build's clients and retry"
    )]
    StaleDaemonsKeepStarting,
    #[error(
        "waited {:?} for another shux client to start the daemon (lock {}); if no other shux \
         command is running, retry",
        start_lock_timeout(), .0.display()
    )]
    StartLockTimeout(std::path::PathBuf),
    #[error("{0}")]
    DaemonInUse(Box<DaemonInUse>),
    #[error(
        "this command was handed to the running daemon's own build ({0}), and that daemon no \
         longer serves the socket; nothing was done. Rerun it"
    )]
    HandoffLost(String),
}

/// Exit code of a command refused with [`ClientError::DaemonInUse`].
pub const DAEMON_IN_USE_EXIT: i32 = 10;

/// `error.code` of that refusal under `--format json`.
pub const DAEMON_IN_USE_CODE: i64 = -32015;

/// A daemon of another build that may hold sessions, left running.
#[derive(Debug)]
pub struct DaemonInUse {
    /// Its build, when it said.
    pub daemon: Option<DaemonVersion>,
    /// Its sessions, scratch included, when they could be counted.
    pub sessions: Option<usize>,
    /// Whether a shux daemon process serves the socket, so `shux daemon stop`
    /// can end it.
    pub stoppable: bool,
    /// The `shux daemon stop` that reaches it, shell-quoted.
    pub stop: String,
}

impl DaemonInUse {
    /// The refusal in `e`'s chain, if it is one.
    pub fn find(e: &anyhow::Error) -> Option<&Self> {
        e.chain()
            .find_map(|c| match c.downcast_ref::<ClientError>() {
                Some(ClientError::DaemonInUse(r)) => Some(&**r),
                _ => None,
            })
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "error": {
                "code": DAEMON_IN_USE_CODE,
                "message": self.to_string(),
                "data": {
                    "kind": "daemon_in_use",
                    "exit_code": DAEMON_IN_USE_EXIT,
                    "daemon_version": self.daemon.as_ref().map(|d| &d.version),
                    "daemon_git_sha": self.daemon.as_ref().map(|d| &d.git_sha),
                    "client_version": caller().0,
                    "client_git_sha": caller().1,
                    "sessions": self.sessions,
                    "stoppable": self.stoppable,
                },
            }
        })
    }
}

impl std::fmt::Display for DaemonInUse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (version, sha) = caller();
        let this = format!("shux {version} ({sha})");
        match &self.daemon {
            Some(dv) => write!(
                f,
                "the running daemon is shux {} ({}) and this CLI is {this}",
                dv.version, dv.git_sha
            )?,
            None => write!(
                f,
                "the running daemon did not say which shux build it is, and this CLI is {this}"
            )?,
        }
        write!(
            f,
            "; {}, so it was left running and this command did not run",
            holding(self.sessions)
        )?;
        if !self.stoppable {
            return write!(
                f,
                "; no shux daemon process serves it, so stop whatever does and retry"
            );
        }
        let why = match self.daemon.as_ref().map(DaemonVersion::compat) {
            Some(Compat::WireBreak) => {
                " (it speaks another protocol, and no copy of its build is \
                                       left to run this command with)"
            }
            _ => "",
        };
        write!(
            f,
            "{why}. The first shux command after its last session ends replaces it with this \
             build. Run `{}` to end its sessions now",
            self.stop,
        )
    }
}

/// Exit code of a call the running daemon, an older build, cannot serve.
pub const DAEMON_TOO_OLD_EXIT: i32 = 11;

/// `error.code` of that refusal under `--format json`.
pub const DAEMON_TOO_OLD_CODE: i64 = -32016;

/// A call refused because the daemon serving it is a build without the method
/// or a parameter it names. Nothing of the call ran.
#[derive(Debug)]
pub struct DaemonTooOld {
    pub peer: Peer,
    pub method: Option<String>,
    /// The parameters it does not take; empty when it lacks the method.
    pub params: Vec<String>,
}

impl DaemonTooOld {
    /// The refusal `e` carries, if it is one: an unknown method or parameter
    /// reported by (or, for 0.64.0, on behalf of) a daemon of another build.
    /// From this very build the same error is a bug, reported as it is.
    pub fn find(e: &anyhow::Error) -> Option<Self> {
        e.chain().find_map(|c| match c.downcast_ref() {
            Some(crate::cli::rpc::RpcClientError::Rpc { code, data, .. }) => {
                Self::from_error(*code, data.as_ref())
            }
            _ => None,
        })
    }

    /// Only a method or key this build itself serves counts: anything else
    /// is a mistake no upgrade fixes.
    pub fn from_error(code: i64, data: Option<&serde_json::Value>) -> Option<Self> {
        let peer = peer()?;
        let field = |k: &str| data.and_then(|d| d.get(k));
        let method = field("method")?.as_str()?.to_string();
        let (_, ours) = crate::rpc::accepts::ACCEPTS
            .iter()
            .find(|(m, _)| *m == method)?;
        let params: Vec<String> = match code {
            METHOD_NOT_FOUND => Vec::new(),
            INVALID_PARAMS
                if field("kind").and_then(|k| k.as_str()) == Some(shux_rpc::UNKNOWN_PARAMS) =>
            {
                let named = field("params")?
                    .as_array()?
                    .iter()
                    .filter_map(|p| p.as_str());
                let known: Vec<String> = named
                    .filter(|p| {
                        ours.contains(p)
                            || p.strip_prefix(OP_FIELD)
                                .is_some_and(|f| shux_core::apply::OP_FIELDS.contains(&f))
                    })
                    .map(String::from)
                    .collect();
                if known.is_empty() {
                    return None;
                }
                known
            }
            _ => return None,
        };
        Some(Self {
            peer,
            method: Some(method),
            params,
        })
    }

    /// What it lacks, in words.
    fn lacks(&self) -> String {
        let method = self
            .method
            .as_deref()
            .unwrap_or("the method this command calls");
        match self.params.as_slice() {
            [] => format!("it has no `{method}`"),
            many => format!("`{method}` there does not take `{}`", many.join("`, `")),
        }
    }

    pub fn to_json(&self) -> serde_json::Value {
        let d = &self.peer.build;
        json!({
            "error": {
                "code": DAEMON_TOO_OLD_CODE,
                "message": self.to_string(),
                "data": {
                    "kind": "daemon_too_old",
                    "exit_code": DAEMON_TOO_OLD_EXIT,
                    "method": self.method,
                    "params": self.params,
                    "daemon_version": d.version,
                    "daemon_git_sha": d.git_sha,
                    "client_version": caller().0,
                    "client_git_sha": caller().1,
                    "upgrade": upgrade_command(&self.peer.socket, "--when-idle"),
                },
            }
        })
    }
}

impl std::fmt::Display for DaemonTooOld {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let d = &self.peer.build;
        write!(
            f,
            "this command needs a newer shux daemon: the running daemon is shux {} ({}) and \
             this CLI is shux {} ({}); {}, so nothing was done. \
             Run `{}` to switch to this build when its last session ends, or `--force` instead \
             of `--when-idle` to end its sessions now",
            d.version,
            d.git_sha,
            caller().0,
            caller().1,
            self.lacks(),
            upgrade_command(&self.peer.socket, "--when-idle"),
        )
    }
}

const INVALID_PARAMS: i64 = -32602;

/// Prefix of a field inside `state.apply` ops, as its refusal names it.
pub(crate) const OP_FIELD: &str = "ops.";

/// `shux daemon upgrade <flag>` for the daemon on `socket`, runtime dir
/// included, as [`stop_command`] names it.
pub(crate) fn upgrade_command(socket: &Path, flag: &str) -> String {
    let stop = stop_command(socket);
    let base = stop.strip_suffix(" daemon stop").unwrap_or(&stop);
    format!("{base} daemon upgrade {flag}")
}

/// The refusal a call to a daemon of 0.64.0 gets before it is sent, when
/// 0.64.0 would not serve it as asked: it ignores parameters it does not know.
pub(crate) fn refuse_for_legacy_peer(
    method: &str,
    params: &serde_json::Value,
) -> Option<(i64, serde_json::Value)> {
    let peer = peer()?;
    if peer.build.compat() != Compat::Legacy064 {
        return None;
    }
    let Some((_, accepted)) = crate::rpc::legacy_0_64::LEGACY_0_64
        .iter()
        .find(|(m, _)| *m == method)
    else {
        // A name this build does not serve either: 0.64.0 says so itself.
        let ours = crate::rpc::accepts::ACCEPTS
            .iter()
            .any(|(m, _)| *m == method);
        return ours.then(|| (METHOD_NOT_FOUND, json!({ "method": method })));
    };
    let mut unknown: Vec<&str> = match params {
        serde_json::Value::Object(o) => o
            .iter()
            .filter(|(k, v)| !accepted.contains(&k.as_str()) && !v.is_null())
            .map(|(k, _)| k.as_str())
            .collect(),
        _ => Vec::new(),
    };
    let mut in_ops = Vec::new();
    if method == "state.apply" {
        legacy_op_fields_dropped(params, &mut in_ops);
    }
    unknown.extend(in_ops.iter().map(String::as_str));
    if unknown.is_empty() {
        return None;
    }
    unknown.sort_unstable();
    unknown.dedup();
    Some((
        INVALID_PARAMS,
        json!({ "kind": shux_rpc::UNKNOWN_PARAMS, "method": method, "params": unknown }),
    ))
}

/// The non-null fields of `state.apply` ops that 0.64.0 would drop, as
/// `ops.<field>`. An op kind it lacks is left to it: it refuses that itself.
fn legacy_op_fields_dropped(params: &serde_json::Value, out: &mut Vec<String>) {
    use crate::rpc::legacy_0_64::{LEGACY_0_64_BACK_REF, LEGACY_0_64_OPS};
    let ops = params.get("ops").and_then(|o| o.as_array());
    for op in ops.into_iter().flatten().filter_map(|op| op.as_object()) {
        let kind = op.get("op").and_then(|k| k.as_str());
        let Some((_, fields)) = LEGACY_0_64_OPS.iter().find(|(k, _)| Some(*k) == kind) else {
            continue;
        };
        for (key, value) in op {
            if value.is_null() {
                continue;
            }
            if !fields.contains(&key.as_str()) {
                out.push(format!("{OP_FIELD}{key}"));
            } else if let Some(back_ref) = value.as_object() {
                out.extend(
                    back_ref
                        .iter()
                        .filter(|(k, v)| {
                            !v.is_null() && !LEGACY_0_64_BACK_REF.contains(&k.as_str())
                        })
                        .map(|(k, _)| format!("{OP_FIELD}{k}")),
                );
            }
        }
    }
}

/// How many sessions a daemon holds, as the refusal and `shux version` say it.
pub(crate) fn holding(sessions: Option<usize>) -> String {
    match sessions {
        Some(0) => "it holds no sessions".to_string(),
        Some(1) => "it holds 1 session".to_string(),
        Some(n) => format!("it holds {n} sessions"),
        None => "its sessions could not be counted".to_string(),
    }
}

/// `shux daemon stop` for the daemon on `socket`, runtime dir included:
/// a bare one stops the default daemon, which may be another.
pub(crate) fn stop_command(socket: &Path) -> String {
    use shux_pty::shell_quote_arg as quote;
    let mut cmd = String::new();
    if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR") {
        cmd.push_str(&format!(
            "XDG_RUNTIME_DIR={} ",
            quote(&dir.to_string_lossy())
        ));
    }
    // Named even when it is the default: an inherited SHUX_SOCKET would redirect it.
    let endpoint = daemon::socket_endpoint(socket);
    cmd.push_str(&format!(
        "shux --socket {} daemon stop",
        quote(&endpoint.to_string_lossy())
    ));
    cmd
}

/// Probe the daemon socket. Returns a connected `UnixStream` if successful.
async fn probe_socket(socket_path: &Path) -> Result<UnixStream, io::Error> {
    UnixStream::connect(socket_path).await
}

/// Version info returned by the daemon's `system.version` RPC call.
#[derive(Debug, Clone)]
pub struct DaemonVersion {
    pub version: String,
    pub git_sha: String,
    /// Its [`shux_rpc::WIRE_GENERATION`]; `None` from builds before it.
    pub wire: Option<u32>,
    /// The whole `system.version` answer.
    pub answer: serde_json::Value,
}

/// How a daemon of some build can serve this CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compat {
    /// This very build.
    Same,
    /// Another build of this wire generation: it serves every call it can,
    /// and refuses the rest one by one.
    Wire,
    /// 0.64.0, which says no generation and ignores unknown parameters: a call
    /// is sent only if 0.64.0 takes every key of it.
    Legacy064,
    /// An older build that says no generation.
    Legacy,
    /// Another wire generation: only its own build can talk to it.
    WireBreak,
}

impl DaemonVersion {
    /// Whether the daemon is this very build, or the build this command was
    /// handed to: a kept copy of that daemon's own binary.
    pub fn is_current(&self) -> bool {
        (self.version == CLIENT_VERSION && self.git_sha == CLIENT_GIT_SHA)
            || crate::kept_build::handed_to()
                .is_some_and(|id| id == format!("{}-{}", self.version, self.git_sha))
    }

    pub fn compat(&self) -> Compat {
        match self.wire {
            _ if self.is_current() => Compat::Same,
            Some(shux_rpc::WIRE_GENERATION) => Compat::Wire,
            Some(_) => Compat::WireBreak,
            None if self.version == "0.64.0" => Compat::Legacy064,
            None => Compat::Legacy,
        }
    }

    /// Whether this CLI is an older release than the daemon: it never
    /// replaces a newer daemon, even an idle one.
    pub fn is_newer_than_this_cli(&self) -> bool {
        match (semver(&self.version), semver(caller().0)) {
            (Some(daemon), Some(cli)) => daemon > cli,
            _ => false,
        }
    }
}

/// `major.minor.patch` of a release version, ignoring any suffix.
pub(crate) fn semver(v: &str) -> Option<(u64, u64, u64)> {
    let core = v.split(['-', '+']).next()?;
    let mut parts = core.split('.').map(|p| p.parse().ok());
    let triple = (parts.next()??, parts.next()??, parts.next()??);
    parts.next().is_none().then_some(triple)
}

/// The daemon of another build this process last connected to, if it did.
#[derive(Debug, Clone)]
pub struct Peer {
    pub build: DaemonVersion,
    pub socket: std::path::PathBuf,
}

static PEER: std::sync::Mutex<Option<Peer>> = std::sync::Mutex::new(None);

/// See [`Peer`]. `None` while every connection was to this very build.
pub fn peer() -> Option<Peer> {
    PEER.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

fn set_peer(peer: Option<Peer>) {
    *PEER.lock().unwrap_or_else(|e| e.into_inner()) = peer;
}

static LAST_DAEMON: std::sync::Mutex<Option<(std::path::PathBuf, Option<u32>)>> =
    std::sync::Mutex::new(None);

/// The socket this process last connected to through
/// [`ensure_daemon_running_at`], and the daemon that served it then.
pub(crate) fn last_daemon() -> Option<(std::path::PathBuf, Option<u32>)> {
    LAST_DAEMON
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

/// Whether daemon `pid` on `socket` is leaving: it exits within
/// [`crate::patience::peer_patience`]. A daemon that is not leaving keeps its
/// state, so a call it dropped is never sent again.
pub(crate) async fn daemon_left(pid: u32, socket: &Path) -> bool {
    let deadline = tokio::time::Instant::now() + crate::patience::peer_patience();
    while crate::daemon_boot::is_live_shux_daemon(pid, socket) {
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        #[expect(
            clippy::disallowed_methods,
            reason = "another process's exit has no event to wait on; poll its identity"
        )]
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    true
}

/// The CLI this command speaks for: the one that handed it off to this
/// build, else this build.
pub(crate) fn caller() -> (&'static str, &'static str) {
    match crate::kept_build::handoff() {
        Some(h) => (&h.caller_version, &h.caller_git_sha),
        None => (CLIENT_VERSION, CLIENT_GIT_SHA),
    }
}

/// This CLI's build, as `system.version` and the retire call take it.
pub(crate) fn client_build() -> serde_json::Value {
    let (version, git_sha) = caller();
    json!({ "client": { "version": version, "git_sha": git_sha } })
}

/// What the daemon serving a socket said when asked which build it is.
pub(crate) enum DaemonAnswer {
    /// Nothing is listening.
    Absent,
    Answered(DaemonVersion),
    /// It answered without saying which build it is.
    Unreadable,
    /// It accepted the connection and did not answer within
    /// [`crate::patience::peer_patience`].
    Unresponsive,
    /// Connecting failed, and not because nothing is listening.
    Unreachable(io::Error),
}

/// Asks the daemon serving `socket_path` which build it is and, when it says,
/// how many sessions it holds, scratch included, on one connection, which is
/// returned when it answered. Never starts or stops one.
pub(crate) async fn ask_daemon_version(
    socket_path: &Path,
) -> (DaemonAnswer, Option<usize>, Option<UnixStream>) {
    let (mut stream, asked) = match ask_version(socket_path, false).await {
        Ok(asked) => asked,
        Err(unanswered) => return (unanswered, None, None),
    };
    let dv = match asked {
        Asked::Result(result) => version_in(&result),
        // On its way out: as good as gone.
        Asked::Error(e) if stopping(&e) => return (DaemonAnswer::Absent, None, None),
        _ => None,
    };
    let Some(dv) = dv else {
        return (DaemonAnswer::Unreadable, None, None);
    };
    let params = json!({ "include_scratch": true });
    let listed = ask(&mut stream, "session-count", "session.list", params);
    let sessions = match tokio::time::timeout(crate::patience::peer_patience(), listed).await {
        Ok(Asked::Result(r)) => r.get("sessions").and_then(|s| s.as_array()).map(Vec::len),
        _ => None,
    };
    (DaemonAnswer::Answered(dv), sessions, Some(stream))
}

/// The connection that asked `socket_path`'s daemon which build it is, and its
/// reply. `Err` holds only `Absent`, `Unresponsive` and `Unreachable`. With
/// `identify` the daemon is told this CLI's build, and notes a newer one.
async fn ask_version(
    socket_path: &Path,
    identify: bool,
) -> Result<(UnixStream, Asked), DaemonAnswer> {
    let mut stream = match probe_socket(socket_path).await {
        Ok(stream) => stream,
        Err(e) if nothing_listening(&e) => return Err(DaemonAnswer::Absent),
        Err(e) => return Err(DaemonAnswer::Unreachable(e)),
    };
    // Builds before 0.65 ignore these params.
    let params = if identify { client_build() } else { json!({}) };
    let asked = ask(&mut stream, "version-check", "system.version", params);
    match tokio::time::timeout(crate::patience::peer_patience(), asked).await {
        Err(_) => Err(DaemonAnswer::Unresponsive),
        Ok(asked) => Ok((stream, asked)),
    }
}

fn version_in(result: &serde_json::Value) -> Option<DaemonVersion> {
    let version = result.get("version")?.as_str()?.to_string();
    let git_sha = result
        .get("git_sha")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    let wire = result
        .get("wire")
        .and_then(|w| w.as_u64())
        .and_then(|w| u32::try_from(w).ok());
    Some(DaemonVersion {
        version,
        git_sha,
        wire,
        answer: result.clone(),
    })
}

/// The reply to one request.
enum Asked {
    Result(serde_json::Value),
    /// A JSON-RPC `error` object.
    Error(serde_json::Value),
    /// The connection ended before a reply.
    Gone,
    /// A reply that is not JSON-RPC.
    Malformed,
}

async fn ask(stream: &mut UnixStream, id: &str, method: &str, params: serde_json::Value) -> Asked {
    let request = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
    let Ok(payload) = serde_json::to_vec(&request) else {
        return Asked::Malformed;
    };
    let sent = async {
        stream
            .write_all(&(payload.len() as u32).to_be_bytes())
            .await?;
        stream.write_all(&payload).await?;
        stream.flush().await?;
        let mut len_buf = [0u8; 4];
        stream.read_exact(&mut len_buf).await?;
        Ok::<_, io::Error>(u32::from_be_bytes(len_buf) as usize)
    };
    let resp_len = match sent.await {
        Ok(n) if n <= 1024 * 1024 => n,
        Ok(_) => return Asked::Malformed,
        Err(_) => return Asked::Gone,
    };
    let mut resp_buf = vec![0u8; resp_len];
    if stream.read_exact(&mut resp_buf).await.is_err() {
        return Asked::Gone;
    }
    let Ok(mut response) = serde_json::from_slice::<serde_json::Value>(&resp_buf) else {
        return Asked::Malformed;
    };
    if let Some(result) = response.get_mut("result") {
        return Asked::Result(result.take());
    }
    match response.get_mut("error") {
        Some(error) => Asked::Error(error.take()),
        None => Asked::Malformed,
    }
}

/// Stop our stale daemon for `socket`, and wait for it to exit.
///
/// Identity rather than bare liveness: a zombie under a non-reaping init still
/// answers `kill(pid, 0)`.
async fn stop_stale_daemon(socket: &Path) -> Result<(), ClientError> {
    let Some(pid) = crate::daemon_boot::our_live_daemon(socket) else {
        return Ok(());
    };
    // ESRCH means it is already gone, which the wait below sees too.
    if nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(pid as i32),
        nix::sys::signal::Signal::SIGTERM,
    )
    .is_ok()
    {
        info!(pid, "Sent SIGTERM to stale daemon");
    }
    let deadline = tokio::time::Instant::now() + crate::patience::peer_patience();
    while crate::daemon_boot::is_live_shux_daemon(pid, socket) {
        if tokio::time::Instant::now() >= deadline {
            return Err(ClientError::StaleDaemonAlive(pid));
        }
        #[expect(
            clippy::disallowed_methods,
            reason = "another process's exit has no event to wait on; poll its identity"
        )]
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    Ok(())
}

/// `" (pid N)"` for our daemon on `socket`, for error messages.
fn holder(socket: &Path) -> String {
    crate::daemon_boot::our_live_daemon(socket)
        .map_or_else(String::new, |pid| format!(" (pid {pid})"))
}

/// What a probe of the socket found.
enum Probe {
    /// A daemon of this build answered on this connection.
    Current(UnixStream),
    /// A daemon answered on this connection, but it is another build, or did
    /// not say which (`None`).
    Stale(Option<DaemonVersion>, UnixStream),
    /// Nothing is listening.
    Absent,
    /// It closed the connection before answering: probe again.
    Gone,
    /// It answered that it is shutting down: wait for it to go.
    Leaving,
    /// Something accepted the connection and never answered.
    Unresponsive,
}

async fn probe_daemon(socket_path: &Path) -> Result<Probe, ClientError> {
    probe(socket_path, false).await
}

async fn probe(socket_path: &Path, confirmed: bool) -> Result<Probe, ClientError> {
    let (stream, asked) = match ask_version(socket_path, true).await {
        Ok(asked) => asked,
        Err(DaemonAnswer::Unreachable(e)) => return Err(ClientError::Io(e)),
        Err(DaemonAnswer::Unresponsive) => return Ok(Probe::Unresponsive),
        Err(_) => return Ok(Probe::Absent),
    };
    let found = match asked {
        Asked::Gone => return Ok(Probe::Gone),
        Asked::Error(e) if stopping(&e) => return Ok(Probe::Leaving),
        Asked::Result(result) => version_in(&result),
        Asked::Error(_) | Asked::Malformed => None,
    };
    match found {
        Some(dv) if dv.is_current() && confirmed => {
            debug!(
                version = CLIENT_VERSION,
                git_sha = CLIENT_GIT_SHA,
                "Daemon version matches"
            );
            Ok(Probe::Current(stream))
        }
        // Asked again on a new connection, which is the one returned: a
        // daemon that answered and then left is not handed back.
        Some(dv) if dv.is_current() => Box::pin(probe(socket_path, true)).await,
        Some(dv) => {
            warn!(
                daemon_version = dv.version,
                daemon_sha = dv.git_sha,
                client_version = CLIENT_VERSION,
                client_sha = CLIENT_GIT_SHA,
                "Daemon version mismatch"
            );
            Ok(Probe::Stale(Some(dv), stream))
        }
        None => {
            warn!("Could not check daemon version");
            Ok(Probe::Stale(None, stream))
        }
    }
}

/// What a daemon of another build agreed to.
enum Verdict {
    /// It refuses new sessions and is shutting down.
    Retiring,
    /// A build without [`shux_rpc::RETIRE_IF_IDLE`] that held no sessions when
    /// asked; only stopping it remains.
    LegacyIdle,
    /// It closed the connection before answering: probe again.
    Gone,
    /// It can serve this CLI, and keeps running: use this connection.
    Serve(UnixStream),
}

/// Asks the daemon of another build on `stream`, the connection that
/// identified it, to retire. One that can serve this CLI ([`Compat::Wire`],
/// [`Compat::Legacy064`]) and holds sessions is served instead; one that is a
/// newer release is never asked. Any other is refused unless it agrees or is
/// a legacy build that is idle.
///
/// A legacy build is counted and then stopped: a session created between the
/// two is lost. That window closes once daemons answer the retire call.
///
/// Without `retire` nothing is asked to change: every daemon is only counted,
/// as a legacy one is.
async fn verdict(
    socket: &Path,
    found: Option<DaemonVersion>,
    mut stream: UnixStream,
    retire: bool,
) -> Result<Verdict, ClientError> {
    let refuse = |daemon, sessions, stoppable| {
        ClientError::DaemonInUse(Box::new(DaemonInUse {
            daemon,
            sessions,
            stoppable,
            stop: stop_command(socket),
        }))
    };
    let ours = || crate::daemon_boot::our_live_daemon(socket).is_some();
    let Some(dv) = found else {
        return Err(unidentified(socket));
    };
    let patience = crate::patience::peer_patience();
    let unresponsive = || ClientError::DaemonUnresponsive(holder(socket), socket.to_path_buf());
    let servable = matches!(dv.compat(), Compat::Wire | Compat::Legacy064);
    if servable && (!retire || dv.is_newer_than_this_cli()) {
        return Ok(Verdict::Serve(stream));
    }
    // An older CLI never swaps a newer daemon for itself, idle or not.
    if dv.is_newer_than_this_cli() {
        return Err(refuse(Some(dv), None, ours()));
    }

    let asked = async {
        if !retire {
            return Ok(Asked::Error(json!({ "code": METHOD_NOT_FOUND })));
        }
        let retire = ask(
            &mut stream,
            "retire",
            shux_rpc::RETIRE_IF_IDLE,
            client_build(),
        );
        tokio::time::timeout(patience, retire).await
    };
    match asked.await {
        Err(_) => return Err(unresponsive()),
        Ok(Asked::Gone) => return Ok(Verdict::Gone),
        Ok(Asked::Error(e)) if stopping(&e) => return Ok(Verdict::Gone),
        Ok(Asked::Result(r)) => {
            let sessions = r.get("sessions").and_then(|n| n.as_u64());
            return match r.get("retired").and_then(|b| b.as_bool()) {
                Some(true) => Ok(Verdict::Retiring),
                _ if servable => Ok(Verdict::Serve(stream)),
                Some(false) if sessions.is_some_and(|n| n > 0) => {
                    Err(refuse(Some(dv), sessions.map(|n| n as usize), ours()))
                }
                _ => Err(refuse(Some(dv), None, ours())),
            };
        }
        Ok(_) if servable => return Ok(Verdict::Serve(stream)),
        Ok(Asked::Error(e)) if e.get("code").and_then(|c| c.as_i64()) == Some(METHOD_NOT_FOUND) => {
        }
        Ok(_) => return Err(refuse(Some(dv), None, ours())),
    }

    let params = json!({ "include_scratch": true });
    let listed = ask(&mut stream, "session-count", "session.list", params);
    let sessions = match tokio::time::timeout(patience, listed).await {
        Err(_) => return Err(unresponsive()),
        Ok(Asked::Gone) => return Ok(Verdict::Gone),
        Ok(Asked::Error(e)) if stopping(&e) => return Ok(Verdict::Gone),
        Ok(Asked::Result(r)) => r.get("sessions").and_then(|s| s.as_array()).map(Vec::len),
        Ok(_) => None,
    };
    let stoppable = ours();
    if sessions == Some(0) && stoppable {
        return Ok(Verdict::LegacyIdle);
    }
    Err(refuse(Some(dv), sessions, stoppable))
}

const METHOD_NOT_FOUND: i64 = -32601;

/// A `daemon_stopping` refusal: the daemon is leaving and ran nothing.
fn stopping(error: &serde_json::Value) -> bool {
    error.pointer("/data/kind").and_then(|k| k.as_str()) == Some(shux_rpc::server::DAEMON_STOPPING)
}

/// Records the daemon of another build this process is about to use.
fn serve(socket: &Path, found: Option<DaemonVersion>) {
    if let Some(build) = found {
        info!(
            daemon_version = build.version,
            daemon_sha = build.git_sha,
            "Using a daemon of another build"
        );
        set_peer(Some(Peer {
            build,
            socket: socket.to_path_buf(),
        }));
    }
}

/// The refusal of a server on `socket` that did not say which build it is.
fn unidentified(socket: &Path) -> ClientError {
    ClientError::DaemonInUse(Box::new(DaemonInUse {
        daemon: None,
        sessions: None,
        stoppable: crate::daemon_boot::our_live_daemon(socket).is_some(),
        stop: stop_command(socket),
    }))
}

/// Under the start lock: the daemon serving `socket`, or `None` once nothing
/// owns it and a new one may start.
///
/// A daemon that stopped serving owns its socket until it exits: it drains its
/// panes first, and holds the instance lock until it has. One whose socket was
/// unlinked rebinds it, and is found by the next probe.
async fn claim_socket(socket: &Path) -> Result<Option<UnixStream>, ClientError> {
    let lock_path = daemon::instance_lock_path_for(socket)?;
    let mut deadline = tokio::time::Instant::now() + crate::patience::peer_patience();
    let mut stops = 0;
    // A departing daemon stops accepting; one that keeps accepting and hanging
    // up is not departing, and is refused on the second hang-up.
    let mut hung_up = false;
    loop {
        let hang_up = |hung_up: &mut bool| {
            if std::mem::replace(hung_up, true) {
                return Err(unidentified(socket));
            }
            Ok(())
        };
        match probe_daemon(socket).await? {
            Probe::Current(stream) => {
                set_peer(None);
                return Ok(Some(stream));
            }
            Probe::Unresponsive => {
                return Err(ClientError::DaemonUnresponsive(
                    holder(socket),
                    socket.to_path_buf(),
                ));
            }
            Probe::Stale(found, stream) => {
                match verdict(socket, found.clone(), stream, true).await? {
                    Verdict::Serve(stream) => {
                        serve(socket, found);
                        return Ok(Some(stream));
                    }
                    // It exits on its own; wait for the socket below.
                    Verdict::Retiring => {}
                    Verdict::Gone => hang_up(&mut hung_up)?,
                    Verdict::LegacyIdle => {
                        if stops == MAX_STALE_STOPS {
                            return Err(ClientError::StaleDaemonsKeepStarting);
                        }
                        stops += 1;
                        stop_stale_daemon(socket).await?;
                        deadline = tokio::time::Instant::now() + crate::patience::peer_patience();
                        continue;
                    }
                }
            }
            Probe::Gone => hang_up(&mut hung_up)?,
            // It said it is going: waited for below, like one that left.
            Probe::Leaving => {}
            Probe::Absent => {
                if daemon::try_lock_file(&lock_path)?.is_some() {
                    return Ok(None);
                }
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(ClientError::DaemonNotServing(holder(socket)));
        }
        #[expect(
            clippy::disallowed_methods,
            reason = "poll interval while a departing daemon still owns the socket"
        )]
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Take the start lock for `socket`, waiting for any client already holding it.
///
/// Polled rather than blocking so a wedged holder costs a bounded wait and a
/// clear error, not a hung CLI.
async fn acquire_start_lock(socket: &Path) -> Result<std::fs::File, ClientError> {
    daemon::ensure_runtime_dir()?;
    let path = daemon::start_lock_path_for(socket)?;
    let deadline = tokio::time::Instant::now() + start_lock_timeout();
    loop {
        if let Some(lock) = daemon::try_lock_file(&path)? {
            return Ok(lock);
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(ClientError::StartLockTimeout(path));
        }
        #[expect(
            clippy::disallowed_methods,
            reason = "polled, not blocking, so a wedged lock holder costs a bounded wait"
        )]
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Ensure the daemon is running at the given socket path and return a connected socket.
///
/// 1. Try to connect to the UDS; a daemon of this build is used as is
/// 2. Otherwise take the start lock, so concurrent clients start one daemon
/// 3. Under the lock, use the daemon another client started, or have a stale
///    one with no sessions retire and wait until nothing owns the socket; one
///    that holds any, or cannot say, is refused
/// 4. Spawn, and retry with exponential backoff until connected
pub async fn ensure_daemon_running_at(socket_path: &Path) -> Result<UnixStream, ClientError> {
    let stream = connect_at(socket_path).await?;
    let pid = crate::daemon_boot::our_live_daemon(socket_path);
    *LAST_DAEMON.lock().unwrap_or_else(|e| e.into_inner()) = Some((socket_path.to_path_buf(), pid));
    Ok(stream)
}

async fn connect_at(socket_path: &Path) -> Result<UnixStream, ClientError> {
    match probe_daemon(socket_path).await? {
        Probe::Current(stream) => {
            set_peer(None);
            return Ok(stream);
        }
        Probe::Unresponsive => {
            return Err(ClientError::DaemonUnresponsive(
                holder(socket_path),
                socket_path.to_path_buf(),
            ));
        }
        // Served or refused before the start lock, so neither waits on it.
        Probe::Stale(found, stream) => {
            if let Verdict::Serve(stream) =
                verdict(socket_path, found.clone(), stream, true).await?
            {
                serve(socket_path, found);
                return Ok(stream);
            }
        }
        Probe::Gone | Probe::Leaving | Probe::Absent => {}
    }
    // A command handed to a daemon's own build is for that daemon alone: it
    // never starts or replaces one.
    if let Some(id) = crate::kept_build::handed_to() {
        return Err(ClientError::HandoffLost(id));
    }

    // Held until this function returns, i.e. until the daemon it started (or
    // found) is accepting connections.
    let _start_lock = acquire_start_lock(socket_path).await?;
    if let Some(stream) = claim_socket(socket_path).await? {
        return Ok(stream);
    }
    info!("Daemon not running, starting...");

    let mut child = start_daemon_process(socket_path)?;
    // The spawned process exits once it has forked the daemon, which then
    // writes its pidfile before it binds.
    let starting = || {
        matches!(child.try_wait(), Ok(None))
            || crate::daemon_boot::our_live_daemon(socket_path).is_some()
    };
    let connected = wait_for_daemon(socket_path, &CONNECT_BUDGET, starting).await;
    // This very build now: nothing of a predecessor's build applies.
    if connected.is_ok() {
        set_peer(None);
    }
    // Reap it: a long-lived client would otherwise keep a zombie. It may
    // still be on its way out, since the daemon can bind before it exits.
    std::thread::spawn(move || child.wait());
    connected
}

/// The refusal [`ensure_daemon_running_at`] would return for `socket_path`, if
/// any, or that the daemon did not answer, for a command that must not write
/// anything first. Starts, stops and retires nothing; any other failure is left
/// for the command's own connect to report.
pub async fn refuse_if_incompatible(socket_path: &Path) -> Result<(), ClientError> {
    let unresponsive =
        || ClientError::DaemonUnresponsive(holder(socket_path), socket_path.to_path_buf());
    match probe_daemon(socket_path).await {
        Ok(Probe::Unresponsive) => Err(unresponsive()),
        Ok(Probe::Stale(found, stream)) => match verdict(socket_path, found, stream, false).await {
            Err(e @ (ClientError::DaemonInUse(_) | ClientError::DaemonUnresponsive(..))) => Err(e),
            _ => Ok(()),
        },
        _ => Ok(()),
    }
}

/// Probe `socket_path` with backoff until the daemon accepts a connection.
async fn wait_for_daemon(
    socket_path: &Path,
    budget: &ConnectBudget,
    mut starting: impl FnMut() -> bool,
) -> Result<UnixStream, ClientError> {
    let deadline = tokio::time::Instant::now() + budget.deadline;
    let mut backoff = budget.backoff;
    let mut attempt = 0;
    loop {
        attempt += 1;
        #[expect(
            clippy::disallowed_methods,
            reason = "connect backoff while the spawned daemon starts"
        )]
        tokio::time::sleep(backoff).await;

        match probe_socket(socket_path).await {
            Ok(stream) => {
                info!(attempt, "Connected to daemon");
                return Ok(stream);
            }
            Err(e) if nothing_listening(&e) => {
                debug!(
                    attempt,
                    backoff_ms = backoff.as_millis(),
                    "Daemon not ready yet, retrying..."
                );
                backoff = (backoff * 2).min(budget.max_backoff);
                if attempt >= budget.retries {
                    if !starting() {
                        return Err(ClientError::ConnectionFailed(attempt));
                    }
                    if tokio::time::Instant::now() >= deadline {
                        return Err(ClientError::DaemonNotStarted(holder(socket_path)));
                    }
                }
            }
            Err(e) => return Err(ClientError::Io(e)),
        }
    }
}

/// Ensure the daemon is running (using default socket path) and return a connected socket.
pub async fn ensure_daemon_running() -> Result<UnixStream, ClientError> {
    let sock_path = daemon::socket_path()?;
    ensure_daemon_running_at(&sock_path).await
}

/// Start the daemon process by re-executing the current binary with
/// an internal `__daemon` subcommand.
///
/// We use re-exec rather than in-process fork+daemonize because the client
/// may already have a tokio runtime running. The `__daemon` subcommand
/// calls `daemonize()` before creating any runtime.
fn start_daemon_process(socket_path: &Path) -> Result<std::process::Child, ClientError> {
    let exe = std::env::current_exe().map_err(ClientError::Io)?;

    let mut cmd = std::process::Command::new(exe);
    cmd.arg("__daemon");
    // The override is the starting client's; the daemon and its panes keep
    // the default.
    cmd.env_remove(crate::patience::PEER_PATIENCE_ENV);

    // Tell the daemon which socket to serve, explicitly: `--socket` is a
    // client-side flag and `SHUX_SOCKET` may be unset. Pass the endpoint,
    // not the spelling: identity checks on platforms with only a space-joined
    // argv match it textually, and a relative path would resolve against the
    // cwd of whoever checks.
    cmd.arg("--socket")
        .arg(daemon::socket_endpoint(socket_path));

    // Detach: don't inherit stdin/stdout/stderr
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::null());

    cmd.spawn().map_err(ClientError::Io)
}

/// Try to connect to an existing daemon without auto-starting.
/// Returns Ok(stream) if daemon is already running, Err otherwise.
pub async fn try_connect(socket_path: &Path) -> Result<UnixStream, ClientError> {
    probe_socket(socket_path).await.map_err(ClientError::Io)
}

/// Whether a failed connect means nothing is listening on the socket. A reset
/// counts: a listener that closes while our connection waits in its backlog, as
/// a retiring daemon's does, resets it, and tokio's `connect` reports that.
fn nothing_listening(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound | io::ErrorKind::ConnectionReset
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A connect the listener reset is absent: on Linux a daemon that stops
    /// listening (as one retiring does) resets the connections still queued on
    /// it, and tokio reports that from `connect` (#311).
    #[test]
    fn a_connection_reset_while_connecting_means_nothing_is_listening() {
        let reset = io::Error::from(io::ErrorKind::ConnectionReset);
        assert!(nothing_listening(&reset));
    }

    /// A daemon that stops serving between the version check and the fresh
    /// connection is absent, not an I/O error the user is shown.
    #[tokio::test]
    async fn a_daemon_gone_after_the_version_check_is_absent() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("d.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let path = socket.clone();
        let server = tokio::spawn(async move {
            let (mut conn, _) = listener.accept().await.unwrap();
            let mut len = [0u8; 4];
            conn.read_exact(&mut len).await.unwrap();
            let mut req = vec![0u8; u32::from_be_bytes(len) as usize];
            conn.read_exact(&mut req).await.unwrap();
            // Stop serving before answering, as a departing daemon does.
            drop(listener);
            std::fs::remove_file(&path).unwrap();
            let resp = serde_json::to_vec(&serde_json::json!({
                "jsonrpc": "2.0",
                "id": "version-check",
                "result": {"version": CLIENT_VERSION, "git_sha": CLIENT_GIT_SHA},
            }))
            .unwrap();
            conn.write_all(&(resp.len() as u32).to_be_bytes())
                .await
                .unwrap();
            conn.write_all(&resp).await.unwrap();
        });
        assert!(matches!(probe_daemon(&socket).await, Ok(Probe::Absent)));
        server.await.unwrap();
    }

    /// A daemon stub on `socket`: on its `n`th connection (from 0) each request
    /// for `method` is answered with `reply(n, method)`, a `result` or `error`
    /// member; `None` closes that connection.
    fn stub(
        socket: &Path,
        reply: impl Fn(usize, &str) -> Option<serde_json::Value> + Send + Sync + 'static,
    ) {
        let listener = tokio::net::UnixListener::bind(socket).unwrap();
        let reply = std::sync::Arc::new(reply);
        tokio::spawn(async move {
            for n in 0.. {
                let Ok((mut conn, _)) = listener.accept().await else {
                    return;
                };
                let reply = reply.clone();
                tokio::spawn(async move {
                    loop {
                        let mut len = [0u8; 4];
                        if conn.read_exact(&mut len).await.is_err() {
                            return;
                        }
                        let mut req = vec![0u8; u32::from_be_bytes(len) as usize];
                        if conn.read_exact(&mut req).await.is_err() {
                            return;
                        }
                        let req: serde_json::Value = serde_json::from_slice(&req).unwrap();
                        let Some(mut resp) = reply(n, req["method"].as_str().unwrap()) else {
                            return;
                        };
                        resp["jsonrpc"] = "2.0".into();
                        resp["id"] = req["id"].clone();
                        let resp = serde_json::to_vec(&resp).unwrap();
                        let framed = [&(resp.len() as u32).to_be_bytes()[..], &resp].concat();
                        if conn.write_all(&framed).await.is_err() {
                            return;
                        }
                    }
                });
            }
        });
    }

    /// This build at its own SHA; otherwise a release from before wire
    /// generations, which a CLI cannot serve itself from while it is busy.
    fn version(sha: &str) -> serde_json::Value {
        let version = if sha == CLIENT_GIT_SHA {
            CLIENT_VERSION
        } else {
            "0.63.0"
        };
        json!({ "result": { "version": version, "git_sha": sha } })
    }

    /// A build of this wire generation.
    fn wire_build(version: &str) -> serde_json::Value {
        json!({ "result": {
            "version": version, "git_sha": "0ther00", "wire": shux_rpc::WIRE_GENERATION,
        } })
    }

    /// `daemon upgrade` asks the daemon it judged, on the connection that
    /// judged it: a daemon that replaced it in between is never asked.
    #[tokio::test]
    async fn daemon_upgrade_asks_the_daemon_it_judged() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("d.sock");
        let asked_on = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = asked_on.clone();
        stub(&socket, move |conn, method| match method {
            "system.version" => Some(wire_build(CLIENT_VERSION)),
            "session.list" => Some(sessions(1)),
            shux_rpc::UPGRADE => {
                seen.lock().unwrap().push(conn);
                Some(json!({ "result": { "pending": true } }))
            }
            _ => None,
        });
        crate::cli::system::handle_daemon_upgrade(
            &socket,
            true,
            false,
            crate::cli::args::OutputFormat::Json,
        )
        .await
        .expect("pending");
        assert_eq!(
            *asked_on.lock().unwrap(),
            vec![0],
            "asked on another connection"
        );
    }

    /// A daemon of this wire generation that holds a session is served, on
    /// the connection that asked, not refused; the CLI remembers it.
    #[tokio::test]
    async fn a_busy_daemon_of_this_wire_generation_is_served() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("d.sock");
        stub(&socket, |_, method| match method {
            "system.version" => Some(wire_build(CLIENT_VERSION)),
            "session.list" => Some(sessions(1)),
            _ => Some(json!({ "result": { "retired": false, "sessions": 1 } })),
        });
        let mut stream = ensure_daemon_running_at(&socket).await.expect("served");
        let listed = ask(&mut stream, "l", "session.list", json!({})).await;
        assert!(
            matches!(listed, Asked::Result(_)),
            "the connection is not usable"
        );
        let peer = peer().expect("the daemon of another build is remembered");
        assert_eq!(peer.build.git_sha, "0ther00");
        assert_eq!(peer.build.compat(), Compat::Wire);
    }

    /// A CLI never asks a newer release to retire, even an idle one: an
    /// older binary run by hand would otherwise swap a fresh daemon for itself.
    #[tokio::test]
    async fn a_newer_release_is_never_asked_to_retire() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("d.sock");
        let asked = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let seen = asked.clone();
        stub(&socket, move |_, method| match method {
            "system.version" => Some(wire_build("999.0.0")),
            "session.list" => Some(sessions(0)),
            _ => {
                seen.store(true, std::sync::atomic::Ordering::SeqCst);
                Some(json!({ "result": { "retired": true, "sessions": 0 } }))
            }
        });
        ensure_daemon_running_at(&socket).await.expect("served");
        assert!(
            !asked.load(std::sync::atomic::Ordering::SeqCst),
            "asked a newer daemon to retire"
        );

        // One this CLI cannot serve itself from is refused, still unasked.
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("d.sock");
        let asked = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let seen = asked.clone();
        stub(&socket, move |_, method| match method {
            "system.version" => Some(json!({ "result": { "version": "999.0.0", "git_sha": "x" } })),
            "session.list" => Some(sessions(0)),
            _ => {
                seen.store(true, std::sync::atomic::Ordering::SeqCst);
                Some(json!({ "result": { "retired": true, "sessions": 0 } }))
            }
        });
        refusal(ensure_daemon_running_at(&socket).await);
        assert!(
            !asked.load(std::sync::atomic::Ordering::SeqCst),
            "asked a newer daemon it cannot serve to retire"
        );
    }

    /// Against 0.64.0, which ignores keys it does not know, a call is sent
    /// only when 0.64.0 takes all of it: a method it lacks or a key it would
    /// drop is refused here, before it is sent. A key set to `null` asks for
    /// nothing and passes.
    #[test]
    fn a_call_0_64_would_not_serve_as_asked_is_refused_before_it_is_sent() {
        set_peer(Some(Peer {
            build: DaemonVersion {
                version: "0.64.0".into(),
                git_sha: "0ther00".into(),
                wire: None,
                answer: json!({}),
            },
            socket: std::path::PathBuf::from("/nonexistent.sock"),
        }));
        let refused = |m, p| refuse_for_legacy_peer(m, &p);
        assert!(refused("pane.capture", json!({ "pane_id": "x", "lines": 5 })).is_none());
        assert!(refused("pane.capture", json!({ "pane_id": "x", "zz_new": null })).is_none());
        let (code, data) = refused("pane.capture", json!({ "pane_id": "x", "zz_new": 1 })).unwrap();
        assert_eq!(
            (code, &data["params"]),
            (INVALID_PARAMS, &json!(["zz_new"]))
        );
        let (code, _) = refused(shux_rpc::UPGRADE, json!({})).unwrap();
        assert_eq!(code, METHOD_NOT_FOUND);
        let template = json!({ "ops": [
            { "op": "create_session", "name": "s", "cwd": "/", "initial_command": ["sh"],
              "initial_window_title": "t" },
            { "op": "create_window", "session": { "ref_op": 0 }, "title": "w", "cwd": null },
            { "op": "split_pane", "target": { "ref_op": 1 }, "direction": "horizontal",
              "ratio": 0.5, "command": [], "cwd": "/", "zz_new": null },
        ] });
        assert!(
            refused("state.apply", template.clone()).is_none(),
            "a template 0.64.0 serves as written"
        );
        let mut newer = template.clone();
        newer["ops"][2]["zz_new"] = json!(1);
        newer["ops"][1]["session"]["zz_ref"] = json!(true);
        let (code, data) = refused("state.apply", newer).unwrap();
        assert_eq!(
            (code, &data["params"]),
            (INVALID_PARAMS, &json!(["ops.zz_new", "ops.zz_ref"])),
            "0.64.0 would drop these op fields, so the batch never reaches it"
        );
        assert!(
            refused("state.apply", json!({ "ops": [{ "op": "zz_op" }] })).is_none(),
            "0.64.0 refuses an op kind it lacks itself"
        );

        assert!(
            DaemonTooOld::from_error(code, Some(&data)).is_none(),
            "an op field this build lacks too is a mistake, not skew"
        );

        let refusal = DaemonTooOld::from_error(
            METHOD_NOT_FOUND,
            Some(&json!({ "method": shux_rpc::UPGRADE })),
        )
        .expect("a refusal while a daemon of another build serves");
        let said = refusal.to_string();
        assert!(
            said.contains("shux 0.64.0 (0ther00)") && said.contains(shux_rpc::UPGRADE),
            "{said}"
        );
        assert!(said.contains("daemon upgrade --when-idle"), "{said}");
        assert_eq!(
            refusal.to_json()["error"]["data"]["exit_code"],
            DAEMON_TOO_OLD_EXIT
        );

        set_peer(None);
        assert!(
            refused("state.apply", json!({})).is_none(),
            "this very build serves anything"
        );
        assert!(
            DaemonTooOld::from_error(METHOD_NOT_FOUND, None).is_none(),
            "an unknown method from this very build is a bug, not skew"
        );
    }

    /// A create refused by a daemon that retired as it arrived (nothing was
    /// applied) is sent once more, to whatever serves the socket next.
    #[tokio::test]
    async fn a_call_a_retiring_daemon_refused_is_sent_to_its_successor() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("d.sock");
        stub(&socket, |n, method| match (n, method) {
            (_, "system.version") => Some(version(CLIENT_GIT_SHA)),
            (1, "session.create") => Some(json!({ "error": {
                "code": -32603, "message": "internal_error",
                "data": { "kind": crate::rpc::convert::DAEMON_RETIRING },
            } })),
            (_, "session.create") => Some(json!({ "result": { "created_on": n } })),
            _ => Some(sessions(0)),
        });
        let mut stream = ensure_daemon_running_at(&socket).await.unwrap();
        let created = crate::cli::rpc::rpc_call(&mut stream, "session.create", json!({}))
            .await
            .expect("sent again");
        assert!(
            created["created_on"].as_u64().is_some_and(|n| n > 1),
            "{created}"
        );
    }

    /// Only what this build itself serves can be blamed on an older daemon:
    /// a method or key this build does not know is a mistake an upgrade
    /// cannot fix, reported as it is.
    #[test]
    fn a_name_this_build_does_not_declare_is_not_skew() {
        set_peer(Some(Peer {
            build: DaemonVersion {
                version: "0.0.1".into(),
                git_sha: "0ther00".into(),
                wire: Some(shux_rpc::WIRE_GENERATION),
                answer: json!({}),
            },
            socket: std::path::PathBuf::from("/nonexistent.sock"),
        }));
        let missing =
            |m: &str| DaemonTooOld::from_error(METHOD_NOT_FOUND, Some(&json!({ "method": m })));
        assert!(
            missing("pane.teleport").is_none(),
            "a typo was blamed on the daemon"
        );
        assert!(
            missing(shux_rpc::UPGRADE).is_some(),
            "a method this build has, the daemon lacks"
        );
        let unknown = |keys: serde_json::Value| {
            DaemonTooOld::from_error(
                INVALID_PARAMS,
                Some(
                    &json!({ "kind": shux_rpc::UNKNOWN_PARAMS, "method": "pane.capture", "params": keys }),
                ),
            )
        };
        assert!(
            unknown(json!(["Pane_id"])).is_none(),
            "a misspelt key was blamed on the daemon"
        );
        let skew = unknown(json!(["lines", "Pane_id"])).expect("a key this build takes");
        assert_eq!(skew.params, vec!["lines".to_string()]);
        let op = |field: &str| {
            DaemonTooOld::from_error(
                INVALID_PARAMS,
                Some(
                    &json!({ "kind": shux_rpc::UNKNOWN_PARAMS, "method": "state.apply", "params": [field] }),
                ),
            )
        };
        assert!(
            op("ops.titel").is_none(),
            "a misspelt op field was blamed on the daemon"
        );
        assert!(
            op("ops.title").is_some(),
            "an op field this build has, the daemon lacks"
        );
        set_peer(None);
    }

    /// A daemon that answers every probe `daemon_stopping` for a while (it
    /// is leaving) is waited out, never refused as unidentified (exit 10).
    #[tokio::test]
    async fn a_daemon_on_its_way_out_is_waited_out_not_refused() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("d.sock");
        stub(&socket, |n, method| match (n, method) {
            (0..=5, _) => Some(json!({ "error": {
                "code": -32603, "message": "internal_error",
                "data": { "kind": shux_rpc::server::DAEMON_STOPPING },
            } })),
            (_, "system.version") => Some(version(CLIENT_GIT_SHA)),
            _ => Some(sessions(0)),
        });
        let connected = ensure_daemon_running_at(&socket).await;
        assert!(connected.is_ok(), "{connected:?}");
    }

    /// A call sent again after a retirement passes the same checks as the
    /// first: a key 0.64.0 would drop is never sent to a 0.64.0 successor.
    #[tokio::test]
    async fn a_resent_call_is_checked_against_the_new_daemon() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("d.sock");
        let got_key = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let seen = got_key.clone();
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        tokio::spawn(async move {
            for n in 0.. {
                let Ok((mut conn, _)) = listener.accept().await else {
                    return;
                };
                let seen = seen.clone();
                tokio::spawn(async move {
                    loop {
                        let mut len = [0u8; 4];
                        if conn.read_exact(&mut len).await.is_err() {
                            return;
                        }
                        let mut req = vec![0u8; u32::from_be_bytes(len) as usize];
                        if conn.read_exact(&mut req).await.is_err() {
                            return;
                        }
                        let req: serde_json::Value = serde_json::from_slice(&req).unwrap();
                        let method = req["method"].as_str().unwrap_or_default();
                        let mut resp = match (n, method) {
                            (0 | 1, "system.version") => version(CLIENT_GIT_SHA),
                            (0 | 1, _) => json!({ "error": {
                                "code": -32603, "message": "internal_error",
                                "data": { "kind": crate::rpc::convert::DAEMON_RETIRING },
                            } }),
                            (_, "system.version") => {
                                json!({ "result": { "version": "0.64.0", "git_sha": "old" } })
                            }
                            (_, "session.create") => {
                                seen.store(
                                    req["params"].get("zz_new").is_some(),
                                    std::sync::atomic::Ordering::SeqCst,
                                );
                                json!({ "result": {} })
                            }
                            _ => json!({ "result": { "retired": false, "sessions": 1 } }),
                        };
                        resp["jsonrpc"] = "2.0".into();
                        resp["id"] = req["id"].clone();
                        let resp = serde_json::to_vec(&resp).unwrap();
                        let framed = [&(resp.len() as u32).to_be_bytes()[..], &resp].concat();
                        if conn.write_all(&framed).await.is_err() {
                            return;
                        }
                    }
                });
            }
        });
        let mut stream = ensure_daemon_running_at(&socket).await.unwrap();
        let params = json!({ "name": "x", "zz_new": 1 });
        let _ = crate::cli::rpc::rpc_call(&mut stream, "session.create", params).await;
        assert!(
            !got_key.load(std::sync::atomic::Ordering::SeqCst),
            "a key 0.64.0 would drop reached it on the resend"
        );
        set_peer(None);
    }

    #[test]
    fn release_versions_compare_by_number() {
        assert_eq!(semver("0.64.0"), Some((0, 64, 0)));
        assert_eq!(semver("1.2.3-rc.1"), Some((1, 2, 3)));
        assert!(semver("0.9.10") > semver("0.9.9"));
        assert_eq!(semver("1.2"), None);
        assert_eq!(semver("1.2.3.4"), None);
        assert_eq!(semver("dev"), None);
    }

    fn sessions(n: usize) -> serde_json::Value {
        json!({ "result": { "sessions": vec![json!({}); n] } })
    }

    fn refusal(r: Result<UnixStream, ClientError>) -> Box<DaemonInUse> {
        match r {
            Err(ClientError::DaemonInUse(refusal)) => refusal,
            other => panic!("expected a daemon_in_use refusal, got {other:?}"),
        }
    }

    /// The sessions weighed are those of the daemon that said which build it
    /// is, asked on that connection: a second connection can reach another
    /// daemon, or the same one after a session arrived.
    #[tokio::test]
    async fn the_sessions_weighed_are_those_of_the_daemon_that_answered() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("d.sock");
        stub(&socket, |n, method| match method {
            "system.version" => Some(version("0ther00")),
            "session.list" => Some(sessions(if n == 0 { 1 } else { 0 })),
            _ => Some(json!({ "result": { "retired": false, "sessions": 1 } })),
        });
        let refused = refusal(ensure_daemon_running_at(&socket).await);
        assert_eq!(refused.sessions, Some(1), "{refused}");
    }

    /// A daemon that closes the connection mid-probe is probed again, not
    /// refused: the one serving the socket now is this build.
    #[tokio::test]
    async fn a_daemon_that_leaves_mid_probe_is_probed_again() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("d.sock");
        stub(&socket, |n, method| match (n, method) {
            (0, "system.version") => Some(version("0ther00")),
            (0, _) => None,
            (_, "system.version") => Some(version(CLIENT_GIT_SHA)),
            _ => Some(sessions(3)),
        });
        let connected = ensure_daemon_running_at(&socket).await;
        assert!(connected.is_ok(), "{connected:?}");
    }

    /// A daemon of this build that answers and then leaves, as a successor
    /// binds, is not the connection handed back.
    #[tokio::test]
    async fn the_connection_returned_is_one_that_still_serves() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("d.sock");
        stub(&socket, |n, method| match (n, method) {
            (_, "system.version") => Some(version(CLIENT_GIT_SHA)),
            (0, _) => None,
            _ => Some(sessions(0)),
        });
        let mut stream = ensure_daemon_running_at(&socket).await.unwrap();
        let listed = ask(&mut stream, "l", "session.list", json!({})).await;
        assert!(
            matches!(listed, Asked::Result(_)),
            "the returned connection was the departed daemon's"
        );
    }

    /// An idle daemon of a build without the retire call, that no shux daemon
    /// process serves, cannot be stopped: refused at once, not waited on.
    #[tokio::test]
    async fn an_idle_legacy_daemon_nothing_can_stop_is_refused_at_once() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("d.sock");
        stub(&socket, |_, method| match method {
            "system.version" => Some(version("0ther00")),
            "session.list" => Some(sessions(0)),
            _ => Some(json!({ "error": { "code": -32601, "message": "method not found" } })),
        });
        let refused = refusal(ensure_daemon_running_at(&socket).await);
        assert_eq!(refused.sessions, Some(0), "{refused}");
        assert!(!refused.stoppable, "{refused}");
    }

    /// A retire answer that is not one is refused, not taken as consent.
    #[tokio::test]
    async fn a_malformed_retire_answer_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("d.sock");
        stub(&socket, |_, method| match method {
            "system.version" => Some(version("0ther00")),
            "session.list" => Some(sessions(0)),
            _ => Some(json!({ "result": { "retired": "yes" } })),
        });
        let refused = refusal(ensure_daemon_running_at(&socket).await);
        assert_eq!(refused.sessions, None, "{refused}");
    }

    /// The recovery command names the socket and runtime dir it is for.
    #[test]
    fn the_stop_command_targets_the_socket_it_is_for() {
        let cmd = stop_command(Path::new("/nonexistent/it's here/d.sock"));
        assert!(
            cmd.ends_with(" --socket '/nonexistent/it'\\''s here/d.sock' daemon stop"),
            "{cmd}"
        );
        // The default socket is named too, so an inherited SHUX_SOCKET cannot
        // redirect the stop to another daemon.
        let socket = daemon::socket_path().unwrap();
        let default = stop_command(&socket);
        let named = shux_pty::shell_quote_arg(&daemon::socket_endpoint(&socket).to_string_lossy());
        assert!(
            default.ends_with(&format!("shux --socket {named} daemon stop")),
            "{default}"
        );
    }

    /// A daemon too busy to answer the version check for 6s is waited for,
    /// not reported as stopped or wedged.
    #[tokio::test]
    async fn a_daemon_slow_to_answer_the_version_check_is_waited_for() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("d.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let path = socket.clone();
        let client = tokio::spawn(async move {
            match ask_version(&path, true).await {
                Ok((_, Asked::Result(r))) => {
                    version_in(&r).map_or(DaemonAnswer::Unreadable, DaemonAnswer::Answered)
                }
                Ok(_) => DaemonAnswer::Unreadable,
                Err(e) => e,
            }
        });
        let (mut conn, _) = listener.accept().await.unwrap();
        let mut len = [0u8; 4];
        conn.read_exact(&mut len).await.unwrap();
        let mut req = vec![0u8; u32::from_be_bytes(len) as usize];
        conn.read_exact(&mut req).await.unwrap();

        // The client is now parked on the reply; let it see the clock move
        // before the reply exists.
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(6)).await;
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        tokio::time::resume();

        let resp = serde_json::to_vec(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": "version-check",
            "result": {"version": CLIENT_VERSION, "git_sha": CLIENT_GIT_SHA},
        }))
        .unwrap();
        // A client that already gave up has closed its end.
        let _ = conn.write_all(&(resp.len() as u32).to_be_bytes()).await;
        let _ = conn.write_all(&resp).await;
        match client.await.unwrap() {
            DaemonAnswer::Answered(dv) => assert!(dv.is_current()),
            DaemonAnswer::Unresponsive => {
                panic!("a daemon that answered after 6s was reported unresponsive")
            }
            _ => panic!("the stub's answer was not read as a version"),
        }
    }

    /// A client stuck holding the start lock (stopped mid-start, say) is given
    /// up on in about the time one daemon replacement takes, not minutes.
    #[tokio::test(start_paused = true)]
    async fn a_stuck_start_lock_holder_is_given_up_on_promptly() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("d.sock");
        let lock_path = daemon::start_lock_path_for(&socket).unwrap();
        std::fs::create_dir_all(lock_path.parent().unwrap()).unwrap();
        let _held = daemon::try_lock_file(&lock_path).unwrap().expect("lock");
        let started = tokio::time::Instant::now();
        let waited = acquire_start_lock(&socket).await;
        let took = started.elapsed();
        assert!(
            matches!(waited, Err(ClientError::StartLockTimeout(..))),
            "{waited:?}"
        );
        // The contract, spelled out: one stale daemon's replacement and a start.
        let bound = crate::patience::peer_patience() * 2 + CONNECT_BUDGET.deadline;
        assert!(
            took <= bound + Duration::from_secs(1),
            "a stuck lock holder held this client for {took:?}, past {bound:?}"
        );
    }

    const QUICK: ConnectBudget = ConnectBudget {
        retries: 3,
        backoff: Duration::from_millis(10),
        max_backoff: Duration::from_millis(20),
        deadline: Duration::from_secs(5),
    };

    /// A daemon that binds after the retries are spent, while it is still
    /// starting, is connected to rather than reported as failed.
    #[tokio::test]
    async fn a_daemon_slower_than_the_retries_is_still_waited_for() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("d.sock");
        // `starting` is asked only once the retries are spent, so binding
        // there orders the bind after them without racing a clock.
        let mut daemon = None;
        let connected = wait_for_daemon(&socket, &QUICK, || {
            daemon.get_or_insert_with(|| std::os::unix::net::UnixListener::bind(&socket).unwrap());
            true
        })
        .await;
        assert!(daemon.is_some(), "the retries were never spent");
        assert!(
            connected.is_ok(),
            "gave up on a starting daemon: {connected:?}"
        );
    }

    /// Once nothing is starting, the retries are all the client waits.
    #[tokio::test]
    async fn a_daemon_that_is_not_starting_fails_after_the_retries() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("d.sock");
        let failed = wait_for_daemon(&socket, &QUICK, || false).await;
        assert!(matches!(failed, Err(ClientError::ConnectionFailed(3))));
    }

    /// A daemon that never binds is given up on at the deadline.
    #[tokio::test]
    async fn a_daemon_that_never_binds_fails_at_the_deadline() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("d.sock");
        let budget = ConnectBudget {
            deadline: Duration::from_millis(300),
            ..QUICK
        };
        let failed = wait_for_daemon(&socket, &budget, || true).await;
        assert!(
            matches!(failed, Err(ClientError::DaemonNotStarted(..))),
            "a daemon still starting at the deadline is named: {failed:?}"
        );
    }
}
