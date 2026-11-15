//! Bounded, redacted record of what went wrong, kept for the saved report.
//!
//! The durable job history (`diagnostics::DiagnosticStore`) keeps only typed
//! codes. When a step fails, maintainers also need the step's failure code
//! and detail, the exit status and output tail of the Wine, installer,
//! helper, or `wineserver` process that failed, the host and HTTP status of
//! a failed download, and when each phase started. Those are appended here,
//! redacted ([`crate::redact`]), as JSON lines in
//! `logs/diagnostics/activity.jsonl`: at most [`FILE_LIMIT`] bytes, with the
//! previous file kept as `activity.1.jsonl`. Files are written atomically and
//! owner-only.
//!
//! Recording never fails the work it describes: until the Launcher opens its
//! diagnostics (and in unit tests) nothing is written.

use std::{
    collections::HashMap,
    ffi::OsStr,
    io,
    path::{Path, PathBuf},
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

use crate::{
    diagnostics::{JobKind, JobStatus},
    operation::WorkPhase,
    persistence::{atomic_write, read_bounded_regular},
    redact::Redactor,
    safe_fs::Directory,
};

pub(crate) const FILE_NAME: &str = "activity.jsonl";
pub(crate) const PREVIOUS_FILE_NAME: &str = "activity.1.jsonl";
/// One file's cap; with the previous file at most twice this is kept.
pub(crate) const FILE_LIMIT: usize = 256 * 1024;
/// The tail of each captured stream kept for a failed process.
pub(crate) const OUTPUT_TAIL: usize = 4 * 1024;
const TEXT_LIMIT: usize = 512;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "event", rename_all = "kebab-case")]
pub(crate) enum ActivityEvent {
    JobStarted {
        kind: JobKind,
        phase: WorkPhase,
    },
    Phase {
        phase: WorkPhase,
    },
    JobFinished {
        kind: JobKind,
        status: JobStatus,
    },
    /// A job's stable failure code, as the result card shows it.
    JobFailed {
        code: String,
    },
    /// The installer's typed error behind a failed step.
    StageFailed {
        code: String,
        detail: String,
    },
    Process {
        label: String,
        /// `exited`, `timed-out`, `stalled` (a download that stopped
        /// changing), `spawn-failed`, or `wait-failed`.
        outcome: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        stdout: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        stderr: String,
    },
    /// A file of the payload Blizzard's downloader left that did not verify:
    /// the payload folder, the file in it and the check it failed (`missing`,
    /// `partial`, `unexpected`, `not-a-file`, `unreadable`, `size`,
    /// `not-a-program`, `sha256`), with the size found for `size`.
    PayloadRejected {
        payload: String,
        file: String,
        check: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        size: Option<u64>,
    },
    Download {
        target: String,
        host: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status: Option<u16>,
        error: String,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct ActivityEntry {
    /// Seconds since the Unix epoch (UTC).
    pub(crate) at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) job: Option<u64>,
    #[serde(flatten)]
    pub(crate) event: ActivityEvent,
    /// The same entry recorded again in a row (a retried download).
    #[serde(default = "one", skip_serializing_if = "is_one")]
    pub(crate) repeat: u32,
}

fn one() -> u32 {
    1
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_one(value: &u32) -> bool {
    *value == 1
}

struct State {
    directory: Option<PathBuf>,
    job: Option<u64>,
    redactor: Option<Redactor>,
}

static STATE: Mutex<State> = Mutex::new(State {
    directory: None,
    job: None,
    redactor: None,
});

/// Start recording into `directory` (`logs/diagnostics`).
pub(crate) fn configure(directory: PathBuf) {
    if let Ok(mut state) = STATE.lock() {
        state.directory = Some(directory);
        state.redactor = Some(Redactor::from_environment());
    }
}

/// The job later entries belong to.
pub(crate) fn set_job(job: Option<u64>) {
    if let Ok(mut state) = STATE.lock() {
        state.job = job;
    }
}

/// Append `event`, redacted. Errors are ignored: a full disk must not turn
/// a download failure into a different failure.
pub(crate) fn record(event: ActivityEvent) {
    record_entry(None, event);
}

/// Append `event` for `job` rather than the current job.
pub(crate) fn record_for(job: u64, event: ActivityEvent) {
    record_entry(Some(job), event);
}

/// Lines kept for the progress view's log, newest last.
const RECENT_LINES: usize = 64;
static RECENT: std::sync::Mutex<std::collections::VecDeque<String>> =
    std::sync::Mutex::new(std::collections::VecDeque::new());

/// Bumped whenever a line is added, so the progress view redraws only then.
static RECENT_VERSION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// The phase the running job is in, and since when.
static CURRENT_PHASE: std::sync::Mutex<Option<(String, std::time::Instant)>> =
    std::sync::Mutex::new(None);

pub(crate) fn recent_version() -> u64 {
    RECENT_VERSION.load(std::sync::atomic::Ordering::Relaxed)
}

/// The running job's phase and how long it has been in it.
pub(crate) fn current_phase() -> Option<(String, std::time::Duration)> {
    CURRENT_PHASE
        .lock()
        .ok()?
        .as_ref()
        .map(|(phase, since)| (phase.clone(), since.elapsed()))
}

/// `text` with what the reports mask (CD keys, the home folder, the user
/// name) masked, for everything shown or printed as it happens.
pub(crate) fn redacted(text: &str) -> String {
    live_redactor().text(text)
}

/// [`redacted`] for a line of the game's own logs: a row of a crash report's
/// memory dump keeps its hex words ([`Redactor::game_log_text`]).
pub(crate) fn redacted_game_log(text: &str) -> String {
    live_redactor().game_log_text(text)
}

fn live_redactor() -> &'static Redactor {
    static LIVE: std::sync::OnceLock<Redactor> = std::sync::OnceLock::new();
    LIVE.get_or_init(Redactor::from_environment)
}

/// One line a running program wrote, for the progress log as it happens.
/// A line of Wine's Winsock channel (Detailed logging) goes to standard
/// error alone, and only when [`winsock_line`] keeps it. The line's class
/// comes first: Wine writes a Winsock line for each call, and the many it
/// drops need no redaction.
pub(crate) fn live_output(line: &str, detailed: bool) {
    let raw = line.trim_end();
    if raw.trim().is_empty() {
        return;
    }
    if NETWORK_LEFT_OUT.load(std::sync::atomic::Ordering::Relaxed) {
        network_lines(|lines, millis| ready(lines.second_passed(millis)));
    }
    socket_tally(raw);
    if raw.contains(":winsock:") {
        network_lines(|lines, millis| lines.winsock_lines(millis, raw));
        return;
    }
    if RECORD_WINDOW_OPEN.load(std::sync::atomic::Ordering::Relaxed) {
        network_lines(|lines, millis| ready(lines.window.passed(millis)));
    }
    let line = redacted(raw);
    let line = line.as_str();
    // Also on standard error, where DroidDeck's session log keeps it. With
    // Detailed logging longer, so a .NET stack trace keeps its lines.
    let limit = output_line_limit(detailed);
    eprintln!("pd2-launcher: | {}", bounded(line, limit));
    if let Ok(mut recent) = RECENT.lock() {
        if recent.len() == RECENT_LINES {
            recent.pop_front();
        }
        recent.push_back(format!("  {}", bounded(line, 300)));
        RECENT_VERSION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// How long a program's output line on standard error may be: with Detailed
/// logging longer, so a .NET stack trace keeps its lines.
const fn output_line_limit(detailed: bool) -> usize {
    if detailed { 1000 } else { 300 }
}

/// What [`live_output`] does with a line of a program's output.
#[derive(Debug, Eq, PartialEq)]
pub(crate) enum WinsockLine {
    /// Not Wine's Winsock channel: shown as it is.
    Other,
    /// A Winsock line of no use (a send, a receive, a wait): left out.
    Dropped,
    /// A Winsock line kept: Wine's process and thread, the function and its
    /// words, without Wine's own clock.
    Kept(String),
}

/// The Winsock functions whose lines Detailed logging keeps: connects and
/// closes, with the address, and name lookups. Errors and warnings are
/// kept from every function.
const WINSOCK_KEPT: [&str; 10] = [
    "connect",
    "WSAConnect",
    "WS2_ConnectEx",
    "closesocket",
    "shutdown",
    "WSASocketW",
    "getaddrinfo",
    "GetAddrInfoW",
    "GetAddrInfoExW",
    "gethostbyname",
];

/// `line` as a line of Wine's Winsock debug channel
/// (`[time:][pid:]tid:class:winsock:function words`), which Detailed logging
/// turns on ([`crate::wine_environment::DETAILED_WINEDEBUG`]): Wine writes one
/// for each call, so most go.
pub(crate) fn winsock_line(line: &str) -> WinsockLine {
    // Wine's own clock (`123.456`) is time since boot: the engine gives its own.
    let Some((ids, class, function, words)) = winsock_parts(line) else {
        return WinsockLine::Other;
    };
    if !winsock_kept(class, function) {
        return WinsockLine::Dropped;
    }
    WinsockLine::Kept(bounded(&winsock_event(&ids, class, function, words), 300))
}

/// Whether Detailed logging keeps a Winsock line outside a record window
/// ([`RecordWindow`]): the functions of [`WINSOCK_KEPT`], and every error and
/// warning.
fn winsock_kept(class: &str, function: &str) -> bool {
    matches!(class, "err" | "warn") || WINSOCK_KEPT.contains(&function)
}

/// A Winsock line's text for the logs: Wine's process and thread, the class
/// for an error or a warning, the function and its words. Not yet redacted
/// and not cut: a line is cut to 300 bytes after its redaction
/// ([`NetLine::render`]).
fn winsock_event(ids: &str, class: &str, function: &str, words: &str) -> String {
    let error = matches!(class, "err" | "warn");
    let mut kept = String::new();
    if !ids.is_empty() {
        kept.push_str(ids);
        kept.push(' ');
    }
    if error {
        kept.push_str(class);
        kept.push(' ');
    }
    kept.push_str(function);
    if !words.is_empty() {
        kept.push(' ');
        kept.push_str(words);
    }
    kept
}

/// A line of Wine's Winsock channel in its parts: Wine's process and thread
/// (`pid:tid`, without Wine's own clock), the class, the function and its
/// words. `None` for any other line.
fn winsock_parts(line: &str) -> Option<(String, &str, &str, &str)> {
    let at = line.find(":winsock:")?;
    let mut head = line[..at].split(':').collect::<Vec<_>>();
    let class = head.pop().unwrap_or_default();
    let rest = &line[at + ":winsock:".len()..];
    let (function, words) = rest.split_once(' ').unwrap_or((rest, ""));
    let ids = head
        .into_iter()
        .filter(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .collect::<Vec<_>>()
        .join(":");
    Some((ids, class, function, words.trim()))
}

/// What one socket did between its connect and its close, from Wine's
/// Winsock lines: the sends and the receives that completed, and when the
/// first and the last answer came. Wine's lines give no byte counts, so a
/// receive counts as one answer (a receive of zero bytes, the peer's close,
/// also counts).
#[derive(Debug)]
struct SocketUse {
    peer: String,
    connected: u64,
    sends: u32,
    answers: u32,
    first_answer: Option<u64>,
    last_answer: Option<u64>,
    failed: u32,
}

/// The sockets of the programs that run, for one summary line at each close
/// ([`SocketTally::line`]): Detailed logging leaves out the send and receive
/// lines, and the summary tells if a server answered at all.
#[derive(Debug, Default)]
pub(crate) struct SocketTally {
    /// The call that each `pid:tid` is in until its status line: a send
    /// (`true`) or a receive, and its socket (`pid socket`).
    calls: HashMap<String, (bool, String)>,
    sockets: HashMap<String, SocketUse>,
}

/// At most this many sockets are followed at the same time; a program that
/// opens more without a close loses the oldest.
const SOCKETS_FOLLOWED: usize = 256;

impl SocketTally {
    /// A Winsock `line` at `millis` (UTC, ms since 1970): the summary line of
    /// a socket that it closes, if any.
    pub(crate) fn line(&mut self, millis: u64, line: &str) -> Option<String> {
        let (ids, _class, function, words) = winsock_parts(line)?;
        let pid = ids.split(':').next().unwrap_or_default().to_owned();
        let socket_of = |words: &str| {
            let socket = words
                .strip_prefix("socket ")
                .unwrap_or(words)
                .split([',', ' '])
                .next()
                .unwrap_or_default();
            format!("{pid} {socket}")
        };
        match function {
            "connect" | "WSAConnect" => {
                let peer = peer_of(words)?;
                if self.sockets.len() >= SOCKETS_FOLLOWED {
                    let oldest = self
                        .sockets
                        .iter()
                        .min_by_key(|(_, used)| used.connected)
                        .map(|(key, _)| key.clone())?;
                    self.sockets.remove(&oldest);
                }
                self.sockets.insert(
                    socket_of(words),
                    SocketUse {
                        peer,
                        connected: millis,
                        sends: 0,
                        answers: 0,
                        first_answer: None,
                        last_answer: None,
                        failed: 0,
                    },
                );
                None
            }
            "WS2_sendto" | "WS2_recv_base" if words.starts_with("socket ") => {
                let socket = socket_of(words);
                if self.sockets.contains_key(&socket) {
                    self.calls.insert(ids, (function == "WS2_sendto", socket));
                }
                None
            }
            "WS2_sendto" | "WS2_recv_base" if words.starts_with("status ") => {
                let (send, socket) = self.calls.remove(&ids)?;
                let used = self.sockets.get_mut(&socket)?;
                let status = words["status ".len()..].trim_end_matches('.');
                if status != "0" && status != "0x0" {
                    used.failed += 1;
                } else if send {
                    used.sends += 1;
                } else {
                    used.answers += 1;
                    used.first_answer.get_or_insert(millis);
                    used.last_answer = Some(millis);
                }
                None
            }
            "closesocket" => {
                let socket = socket_of(words);
                let used = self.sockets.remove(&socket)?;
                self.calls.retain(|_, (_, of)| *of != socket);
                let after = |at: u64| at.saturating_sub(used.connected) as f64 / 1000.0;
                let mut summary = format!(
                    "{ids} socket {} to {} closed after {:.2} s: {} sends, {} answers",
                    socket.split(' ').nth(1).unwrap_or_default(),
                    used.peer,
                    after(millis),
                    used.sends,
                    used.answers
                );
                if let (Some(first), Some(last)) = (used.first_answer, used.last_answer) {
                    summary.push_str(&format!(
                        " (first after {:.2} s, last after {:.2} s)",
                        after(first),
                        after(last)
                    ));
                }
                if used.failed > 0 {
                    summary.push_str(&format!(", {} calls failed", used.failed));
                }
                Some(summary)
            }
            _ => None,
        }
    }
}

/// `address:port` of a connect's words (`socket 0x1a4, addr { family
/// AF_INET, address 198.51.100.7, port 6112 }, len 16`).
fn peer_of(words: &str) -> Option<String> {
    let address = words.split("address ").nth(1)?.split([',', ' ']).next()?;
    let port = words.split("port ").nth(1)?.split([',', ' ', '}']).next()?;
    Some(format!("{address}:{port}"))
}

static SOCKET_TALLY: std::sync::LazyLock<std::sync::Mutex<SocketTally>> =
    std::sync::LazyLock::new(Default::default);

/// [`SocketTally::line`] on a line of a running program: a socket's summary
/// goes out as a network line.
fn socket_tally(line: &str) {
    if !line.contains(":winsock:") {
        return;
    }
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_millis() as u64);
    let summary = SOCKET_TALLY
        .lock()
        .ok()
        .and_then(|mut tally| tally.line(millis, line));
    if let Some(summary) = summary {
        let summary = redacted(&summary);
        network_lines(|lines, millis| ready(lines.event(millis, &summary)));
    }
}

/// A network line to write: ready, or a head and a text that is redacted,
/// then cut to `limit` bytes, only after the state's lock is released
/// ([`network_lines`]): the redaction of a window's first lines takes time,
/// and Wine's output waits for none of it under the lock.
#[derive(Debug)]
pub(crate) enum NetLine {
    Ready(String),
    Redact {
        head: String,
        text: String,
        limit: Option<usize>,
    },
}

impl NetLine {
    /// The line as written: `redact` first, then the cut.
    fn render(self, redact: &impl Fn(&str) -> String) -> String {
        match self {
            Self::Ready(line) => line,
            Self::Redact { head, text, limit } => {
                let text = redact(&text);
                match limit {
                    Some(limit) => head + &bounded(&text, limit),
                    None => head + &text,
                }
            }
        }
    }
}

fn ready(lines: Vec<String>) -> Vec<NetLine> {
    lines.into_iter().map(NetLine::Ready).collect()
}

/// `lines` as written, one text with a line end after each, in their order.
fn render_all(lines: Vec<NetLine>, redact: &impl Fn(&str) -> String) -> String {
    let mut text = String::new();
    for line in lines {
        text.push_str(&line.render(redact));
        text.push('\n');
    }
    text
}

/// At most this many network lines a second; the rest are counted.
const NETWORK_LINES_PER_SECOND: u32 = 20;

/// The network lines of one wall-clock second: those written, and those left
/// out, whose count goes out once that second is over (at the next line of
/// any program, or when a program's output ends) with the second it
/// belongs to.
#[derive(Debug, Default)]
pub(crate) struct NetworkLines {
    second: u64,
    lines: u32,
    left_out: u32,
    window: RecordWindow,
}

impl NetworkLines {
    /// The lines to write for a line of Wine's Winsock channel, `raw`, at
    /// `millis` (UTC, ms since 1970). In a record window ([`RecordWindow`])
    /// every Winsock line goes out, with no limit; outside one, only those
    /// that [`winsock_line`] keeps, at most [`NETWORK_LINES_PER_SECOND`] a
    /// second, and each line waits [`WINDOW_BEFORE_MS`] in the window's
    /// memory. The lines are redacted after the state's lock is released
    /// ([`network_lines`]).
    pub(crate) fn winsock_lines(&mut self, millis: u64, raw: &str) -> Vec<NetLine> {
        let mut out = ready(self.window.passed(millis));
        let Some((ids, class, function, words)) = winsock_parts(raw) else {
            return out;
        };
        let poll = Poll::of(class, function);
        let event = winsock_event(&ids, class, function, words);
        if let Some(peer) = window_trigger(function, words) {
            out.extend(self.window.open(millis, &ids, &peer));
        }
        if self.window.is_open() {
            out.extend(self.window.line(millis, poll, event));
            return out;
        }
        if winsock_kept(class, function) {
            self.window.remember(millis, Before::Line(event.clone()));
            out.extend(self.kept(millis, event, true));
        } else {
            self.window
                .remember(millis, poll.map_or(Before::Line(event), Before::Poll));
        }
        out
    }

    /// [`Self::winsock_lines`] with `redact` on each line, as written.
    #[cfg(test)]
    pub(crate) fn winsock(
        &mut self,
        millis: u64,
        raw: &str,
        redact: impl Fn(&str) -> String,
    ) -> Vec<String> {
        self.winsock_lines(millis, raw)
            .into_iter()
            .map(|line| line.render(&redact))
            .collect()
    }

    /// The lines to write for a kept Winsock `event` at `millis` (UTC, ms
    /// since 1970), already redacted.
    pub(crate) fn event(&mut self, millis: u64, event: &str) -> Vec<String> {
        self.kept(millis, event.to_owned(), false)
            .into_iter()
            .map(|line| line.render(&|text: &str| text.to_owned()))
            .collect()
    }

    /// A kept line `text` at `millis`, at most [`NETWORK_LINES_PER_SECOND`]
    /// a second; `redact`: it still needs its redaction.
    fn kept(&mut self, millis: u64, text: String, redact: bool) -> Vec<NetLine> {
        let mut out = ready(self.second_passed(millis));
        let second = millis / 1000;
        if self.lines > 0 && second == self.second {
            if self.lines >= NETWORK_LINES_PER_SECOND {
                self.left_out += 1;
                return out;
            }
            self.lines += 1;
        } else {
            self.second = second;
            self.lines = 1;
        }
        let head = format!(
            "pd2-launcher: network {} ",
            crate::report::utc_time_millis(millis)
        );
        out.push(if redact {
            NetLine::Redact {
                head,
                text,
                limit: Some(300),
            }
        } else {
            NetLine::Ready(head + &text)
        });
        out
    }

    /// The count of the lines left out, once their second is over at
    /// `millis`.
    pub(crate) fn second_passed(&mut self, millis: u64) -> Vec<String> {
        if millis / 1000 == self.second {
            return Vec::new();
        }
        self.flush().into_iter().collect()
    }

    /// The count of the lines left out so far, if any.
    pub(crate) fn flush(&mut self) -> Option<String> {
        if self.left_out == 0 {
            return None;
        }
        let line = format!(
            "pd2-launcher: network: {} more lines in the second from {} were left out",
            self.left_out,
            crate::report::utc_time_millis(self.second * 1000)
        );
        self.left_out = 0;
        Some(line)
    }

    fn left_out(&self) -> bool {
        self.left_out > 0
    }
}

/// The time before a record window's connect whose Winsock lines the window
/// writes first, in ms.
const WINDOW_BEFORE_MS: u64 = 10_000;
/// A record window's length from a connect, in ms.
const WINDOW_MS: u64 = 20_000;
/// A record window's longest length from its first connect, in ms: a realm
/// connect in an open window moves its end, but not past this.
const WINDOW_MAX_MS: u64 = 30_000;
/// The same for a game-server connect (port 4000): the window is for the
/// game-server join, so its 20 s are never cut by an earlier realm connect;
/// only a second game-server connect can reach this bound.
const WINDOW_GAME_MAX_MS: u64 = 45_000;
/// At most this many Winsock lines wait for a window: the oldest go first.
const WINDOW_BEFORE_LINES: usize = 20_000;

/// The connect that opens a record window: to a game server (port 4000) or
/// to the realm (port 6113). Its peer, `address:port`.
fn window_trigger(function: &str, words: &str) -> Option<String> {
    if !matches!(function, "connect" | "WSAConnect") {
        return None;
    }
    let peer = peer_of(words)?;
    (peer.ends_with(":4000") || peer.ends_with(":6113")).then_some(peer)
}

/// A Winsock call that the game makes many times a second to wait for and
/// look at its sockets: a record window counts these each second and writes
/// one line for them ([`PollCount`]); an error or a warning of one is
/// written as it is.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Poll {
    Select,
    SockIoctl,
}

impl Poll {
    fn of(class: &str, function: &str) -> Option<Self> {
        if matches!(class, "err" | "warn") {
            return None;
        }
        match function {
            "select" => Some(Self::Select),
            "sock_ioctl" => Some(Self::SockIoctl),
            _ => None,
        }
    }
}

/// A Winsock line that waits for a window: its text, not yet redacted, or
/// only the [`Poll`] it was.
#[derive(Debug)]
enum Before {
    Line(String),
    Poll(Poll),
}

/// The [`Poll`] calls of one wall-clock second: one line once the second is
/// over.
#[derive(Debug, Default)]
struct PollCount {
    second: u64,
    select: u32,
    sock_ioctl: u32,
}

impl PollCount {
    /// The line of the second before, when `millis` is in a later second.
    fn at(&mut self, millis: u64) -> Option<String> {
        if millis / 1000 == self.second {
            return None;
        }
        let line = self.flush();
        self.second = millis / 1000;
        line
    }

    fn count(&mut self, poll: Poll) {
        match poll {
            Poll::Select => self.select += 1,
            Poll::SockIoctl => self.sock_ioctl += 1,
        }
    }

    /// `select ×48, sock_ioctl ×12 in the second from <time>`, if any.
    fn flush(&mut self) -> Option<String> {
        let mut parts = Vec::new();
        if self.select > 0 {
            parts.push(format!("select ×{}", self.select));
        }
        if self.sock_ioctl > 0 {
            parts.push(format!("sock_ioctl ×{}", self.sock_ioctl));
        }
        self.select = 0;
        self.sock_ioctl = 0;
        (!parts.is_empty()).then(|| {
            format!(
                "pd2-launcher: network window: {} in the second from {}",
                parts.join(", "),
                crate::report::utc_time_millis(self.second * 1000)
            )
        })
    }
}

/// A record window of Detailed logging (issue #17: the game closes a game
/// server's socket about 15 s after its connect, and the kept lines do not
/// show what came before). A connect to a game server or to the realm
/// ([`window_trigger`]) opens it: it writes the Winsock lines of the
/// [`WINDOW_BEFORE_MS`] before, then for [`WINDOW_MS`] every Winsock line
/// (sends, receives, connects, closes, lookups, errors) with no limit a
/// second, but the [`Poll`] calls as one count a second. A connect in an open
/// window moves its end to [`WINDOW_MS`] after it: a realm connect at most to
/// [`WINDOW_MAX_MS`] after the window's first connect, a game-server connect
/// at most to [`WINDOW_GAME_MAX_MS`] (so a game-server connect up to 25 s
/// after the first connect keeps its full [`WINDOW_MS`]); after the end, the
/// next such connect opens a new window. Each line shows its time from the window's first
/// connect. Wine's lines name sockets, buffers and sizes of address
/// structures, never the data.
#[derive(Debug, Default)]
pub(crate) struct RecordWindow {
    /// The Winsock lines of the last [`WINDOW_BEFORE_MS`], not yet redacted:
    /// most are never written.
    before: std::collections::VecDeque<(u64, Before)>,
    opened: u64,
    /// The window's end; 0 when no window is open.
    until: u64,
    lines: u64,
    polls: u64,
    count: PollCount,
    /// The first game-server connect in the window, if any: it has its full
    /// [`WINDOW_MS`]; a second one ends the window at most
    /// [`WINDOW_GAME_MAX_MS`] after it.
    game: Option<u64>,
    /// The output stream (its drain thread) whose line opened the window:
    /// the end of that program's output ends the window
    /// ([`RecordWindow::ended`]).
    opener: Option<std::thread::ThreadId>,
}

impl RecordWindow {
    fn is_open(&self) -> bool {
        self.until > 0
    }

    fn forget_before(&mut self, millis: u64) {
        let from = millis.saturating_sub(WINDOW_BEFORE_MS);
        while self.before.front().is_some_and(|(at, _)| *at < from) {
            self.before.pop_front();
        }
    }

    fn remember(&mut self, millis: u64, line: Before) {
        self.forget_before(millis);
        if self.before.len() >= WINDOW_BEFORE_LINES {
            self.before.pop_front();
        }
        self.before.push_back((millis, line));
    }

    /// Open a window at a connect of `ids` to `peer` at `millis`: its first
    /// line and the lines before. A connect in an open window moves its end:
    /// the window's first game-server connect to its full [`WINDOW_MS`]; a
    /// later one to at most [`WINDOW_GAME_MAX_MS`] after that first one; a
    /// realm connect to at most [`WINDOW_MAX_MS`] after the window's first
    /// connect. The end never moves back.
    fn open(&mut self, millis: u64, ids: &str, peer: &str) -> Vec<NetLine> {
        let game = peer.ends_with(":4000");
        if self.is_open() {
            let (end, why) = match (game, self.game) {
                (true, None) => {
                    self.game = Some(millis);
                    (
                        millis + WINDOW_MS,
                        format!(
                            "its first game-server connect: its full {} s",
                            WINDOW_MS / 1000
                        ),
                    )
                }
                (true, Some(first)) => (
                    (millis + WINDOW_MS).min(first + WINDOW_GAME_MAX_MS),
                    format!(
                        "at most {} s after its first game-server connect",
                        WINDOW_GAME_MAX_MS / 1000
                    ),
                ),
                (false, _) => (
                    (millis + WINDOW_MS).min(self.opened + WINDOW_MAX_MS),
                    format!("at most {} s after its first connect", WINDOW_MAX_MS / 1000),
                ),
            };
            self.until = self.until.max(end);
            return vec![NetLine::Redact {
                head: String::new(),
                text: format!(
                    "pd2-launcher: network window {}: {ids} connect to {peer}: the window ends at {} ({why})",
                    self.offset(millis),
                    self.offset(self.until),
                ),
                limit: None,
            }];
        }
        self.forget_before(millis);
        self.opened = millis;
        self.until = millis + WINDOW_MS;
        self.lines = 0;
        self.polls = 0;
        self.count = PollCount::default();
        self.game = game.then_some(millis);
        self.opener = Some(std::thread::current().id());
        let before = std::mem::take(&mut self.before);
        let mut out = vec![NetLine::Redact {
            head: String::new(),
            text: format!(
                "pd2-launcher: network window: opened at {} by {ids} connect to {peer}: the {} Winsock lines \
                 of the {} s before, then every Winsock line for {} s (select and sock_ioctl counted each \
                 second)",
                crate::report::utc_time_millis(millis),
                before.len(),
                WINDOW_BEFORE_MS / 1000,
                WINDOW_MS / 1000
            ),
            limit: None,
        }];
        let mut count = PollCount::default();
        for (at, line) in before {
            out.extend(count.at(at).map(NetLine::Ready));
            match line {
                Before::Poll(poll) => count.count(poll),
                Before::Line(event) => out.push(NetLine::Redact {
                    head: format!("pd2-launcher: network window {} ", self.offset(at)),
                    text: event,
                    limit: Some(300),
                }),
            }
        }
        out.extend(count.flush().map(NetLine::Ready));
        out
    }

    /// The lines for a Winsock line `event` in the open window at `millis`:
    /// a [`Poll`] is counted, any other line written.
    fn line(&mut self, millis: u64, poll: Option<Poll>, event: String) -> Vec<NetLine> {
        let mut out = self
            .count
            .at(millis)
            .map(NetLine::Ready)
            .into_iter()
            .collect::<Vec<_>>();
        if let Some(poll) = poll {
            self.polls += 1;
            self.count.count(poll);
        } else {
            self.lines += 1;
            out.push(NetLine::Redact {
                head: format!("pd2-launcher: network window {} ", self.offset(millis)),
                text: event,
                limit: Some(300),
            });
        }
        out
    }

    /// `millis` from the window's first connect: `+1.250 s`, `-3.000 s`.
    fn offset(&self, millis: u64) -> String {
        let millis = millis as i64 - self.opened as i64;
        format!("{:+.3} s", millis as f64 / 1000.0)
    }

    /// The window's last lines, once its end has passed at `millis`.
    pub(crate) fn passed(&mut self, millis: u64) -> Vec<String> {
        if !self.is_open() || millis < self.until {
            return Vec::new();
        }
        self.close()
    }

    /// The open window's last lines (the count of its last second, then its
    /// end), if a window is open.
    pub(crate) fn close(&mut self) -> Vec<String> {
        self.close_at(self.until, "")
    }

    /// The end of the output of the program whose drain thread is `stream`,
    /// at `millis`: the window's last lines if that program opened it (with
    /// the time it was really open), or if its end has passed.
    pub(crate) fn ended(&mut self, millis: u64, stream: std::thread::ThreadId) -> Vec<String> {
        if !self.is_open() || millis >= self.until {
            return self.passed(millis);
        }
        if self.opener != Some(stream) {
            return Vec::new();
        }
        self.close_at(millis, " (its program's output ended)")
    }

    fn close_at(&mut self, end: u64, why: &str) -> Vec<String> {
        if !self.is_open() {
            return Vec::new();
        }
        let mut out = self.count.flush().into_iter().collect::<Vec<_>>();
        out.push(format!(
            "pd2-launcher: network window: closed {:.1} s after its open at {}{why}: {} Winsock lines and {} \
             select and sock_ioctl calls in it",
            end.saturating_sub(self.opened) as f64 / 1000.0,
            crate::report::utc_time_millis(self.opened),
            self.lines,
            self.polls
        ));
        self.until = 0;
        self.game = None;
        self.opener = None;
        out
    }
}

static NETWORK_LINES: std::sync::Mutex<NetworkLines> = std::sync::Mutex::new(NetworkLines {
    second: 0,
    lines: 0,
    left_out: 0,
    window: RecordWindow {
        before: std::collections::VecDeque::new(),
        opened: 0,
        until: 0,
        lines: 0,
        polls: 0,
        count: PollCount {
            second: 0,
            select: 0,
            sock_ioctl: 0,
        },
        game: None,
        opener: None,
    },
});
/// Whether a record window is open: each output line then looks whether its
/// end has passed.
static RECORD_WINDOW_OPEN: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
/// Whether [`NETWORK_LINES`] has a count to write: each output line then
/// looks whether its second is over.
static NETWORK_LEFT_OUT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The turn to write network lines: taken under [`NETWORK_LINES`]'s lock and
/// held while the lines are redacted and written, so lines of two threads
/// keep their order, while the state is free for the next line.
static NETWORK_OUTPUT: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// `step` on the network lines at the time now, under their lock; then,
/// with the lock released, its lines are redacted and go to standard error
/// as one write.
fn network_lines(step: impl FnOnce(&mut NetworkLines, u64) -> Vec<NetLine>) {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_millis() as u64);
    let (pending, _turn) = {
        let Ok(mut lines) = NETWORK_LINES.lock() else {
            return;
        };
        let pending = step(&mut lines, millis);
        NETWORK_LEFT_OUT.store(lines.left_out(), std::sync::atomic::Ordering::Relaxed);
        RECORD_WINDOW_OPEN.store(lines.window.is_open(), std::sync::atomic::Ordering::Relaxed);
        if pending.is_empty() {
            return;
        }
        let turn = NETWORK_OUTPUT
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        (pending, turn)
    };
    let text = render_all(pending, &redacted);
    let _ = std::io::Write::write_all(&mut std::io::stderr().lock(), text.as_bytes());
}

/// A program's output stream ended (it stopped): the count of network lines
/// left out so far, and the last lines of a record window that this stream
/// opened or whose end has passed.
pub(crate) fn output_ended() {
    if NETWORK_LEFT_OUT.load(std::sync::atomic::Ordering::Relaxed)
        || RECORD_WINDOW_OPEN.load(std::sync::atomic::Ordering::Relaxed)
    {
        let stream = std::thread::current().id();
        network_lines(|lines, millis| {
            let mut out = lines.window.ended(millis, stream);
            out.extend(lines.flush());
            ready(out)
        });
    }
}

/// What the capture that a program's result keeps ([`crate::host_execution`])
/// does with a line of its output.
#[derive(Debug, Eq, PartialEq)]
pub(crate) enum CaptureLine {
    Keep,
    /// Wine's `+loaddll` lines and the Winsock lines that [`winsock_line`]
    /// drops: the capture of a failed program then holds its error.
    Drop,
    /// A Winsock line that [`winsock_line`] keeps: at most
    /// [`NETWORK_LINES_PER_SECOND`] a second ([`CaptureNetworkLines`]).
    Network,
}

/// The capture's [`CaptureLine`] for `line`. `engine.log` has every line
/// but the dropped Winsock lines.
pub(crate) fn capture_line(line: &[u8]) -> CaptureLine {
    let line = String::from_utf8_lossy(line);
    if line.contains(":trace:loaddll:") {
        return CaptureLine::Drop;
    }
    match winsock_line(&line) {
        WinsockLine::Other => CaptureLine::Keep,
        WinsockLine::Dropped => CaptureLine::Drop,
        WinsockLine::Kept(_) => CaptureLine::Network,
    }
}

/// The network lines one capture takes in a second of the wall clock.
#[derive(Debug, Default)]
pub(crate) struct CaptureNetworkLines {
    second: u64,
    lines: u32,
}

impl CaptureNetworkLines {
    /// Whether the capture takes a network line at `second` (seconds since
    /// 1970).
    pub(crate) fn take(&mut self, second: u64) -> bool {
        if second != self.second {
            self.second = second;
            self.lines = 0;
        }
        self.lines += 1;
        self.lines <= NETWORK_LINES_PER_SECOND
    }
}

/// The last `count` lines of what the running job did, for the log under its
/// progress bar.
pub(crate) fn recent_lines(count: usize) -> Vec<String> {
    RECENT.lock().map_or_else(
        |_| Vec::new(),
        |recent| {
            recent
                .iter()
                .skip(recent.len().saturating_sub(count))
                .cloned()
                .collect()
        },
    )
}

/// Test seam: add one line to the progress log.
#[cfg(test)]
pub(crate) fn push_recent_for_test(line: &str) {
    if let Ok(mut recent) = RECENT.lock() {
        recent.push_back(line.to_owned());
    }
}

fn note(event: &ActivityEvent) {
    let Ok(mut recent) = RECENT.lock() else {
        return;
    };
    if matches!(event, ActivityEvent::JobStarted { .. }) {
        recent.clear();
    }
    if let Ok(mut current) = CURRENT_PHASE.lock() {
        match event {
            ActivityEvent::JobStarted { phase, .. } | ActivityEvent::Phase { phase } => {
                *current = Some((words(&format!("{phase:?}")), std::time::Instant::now()));
            }
            ActivityEvent::JobFinished { .. } => *current = None,
            _ => {}
        }
    }
    RECENT_VERSION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut push = |line: String| {
        if recent.len() == RECENT_LINES {
            recent.pop_front();
        }
        recent.push_back(line);
    };
    match event {
        ActivityEvent::JobStarted { kind, phase } => {
            push(format!(
                "{} started: {}",
                words(&format!("{kind:?}")),
                words(&format!("{phase:?}"))
            ));
        }
        ActivityEvent::Phase { phase } => push(words(&format!("{phase:?}"))),
        ActivityEvent::JobFinished { kind, status } => {
            push(format!(
                "{} {}",
                words(&format!("{kind:?}")),
                words(&format!("{status:?}")).to_lowercase()
            ));
        }
        ActivityEvent::JobFailed { code } | ActivityEvent::StageFailed { code, .. } => {
            push(format!("Failed: {code}"));
        }
        ActivityEvent::Process {
            label,
            outcome,
            exit_code,
            stdout,
            stderr,
        } => {
            push(format!(
                "{label} {outcome}{}",
                exit_code.map_or_else(String::new, |code| format!(" (exit code {code})"))
            ));
            let lines = stdout
                .lines()
                .chain(stderr.lines())
                .filter(|line| !line.trim().is_empty());
            let lines = lines.collect::<Vec<_>>();
            for line in &lines[lines.len().saturating_sub(6)..] {
                push(format!("  {line}"));
            }
        }
        ActivityEvent::PayloadRejected {
            payload,
            file,
            check,
            size,
        } => push(format!(
            "{payload}: {file} failed the {check} check{}",
            size.map_or_else(String::new, |size| format!(" ({size} bytes)"))
        )),
        ActivityEvent::Download {
            target,
            host,
            status,
            error,
        } => push(format!(
            "Download of {target} from {host} failed: {error}{}",
            status.map_or_else(String::new, |status| format!(" (HTTP {status})"))
        )),
    }
}

/// `PrefixInitialization` as "Prefix initialization".
fn words(name: &str) -> String {
    let mut text = String::new();
    for (index, character) in name.chars().enumerate() {
        if character.is_uppercase() && index > 0 {
            text.push(' ');
            text.extend(character.to_lowercase());
        } else {
            text.push(character);
        }
    }
    text
}

fn record_entry(job: Option<u64>, event: ActivityEvent) {
    note(&redact_event(event.clone(), live_redactor()));
    let Ok(state) = STATE.lock() else {
        return;
    };
    let (Some(directory), Some(redactor)) = (&state.directory, &state.redactor) else {
        return;
    };
    let entry = ActivityEntry {
        at: now(),
        job: job.or(state.job),
        event: redact_event(event, redactor),
        repeat: 1,
    };
    let _ = append(directory, entry);
}

/// A failed process: its label, how it ended, and the tail of what it wrote.
/// A downloaded payload's file that did not verify ([`ActivityEvent::PayloadRejected`]).
pub(crate) fn record_payload_problem(payload: &str, file: &str, check: &str, size: Option<u64>) {
    eprintln!(
        "pd2-launcher: payload {}: {} failed the {check} check",
        redacted(payload),
        redacted(file)
    );
    record(ActivityEvent::PayloadRejected {
        payload: payload.to_owned(),
        file: file.to_owned(),
        check: check.to_owned(),
        size,
    });
}

pub(crate) fn record_process(
    label: &str,
    outcome: &str,
    exit_code: Option<i32>,
    output: &crate::host_execution::ProcessOutput,
) {
    // Also on standard error: on some systems (DroidDeck) that is the only
    // log anyone can read.
    eprintln!(
        "pd2-launcher: {label} {outcome} (exit code {})",
        exit_code.map_or_else(|| "none".to_owned(), |code| code.to_string())
    );
    for (stream, bytes) in [("stdout", output.stdout()), ("stderr", output.stderr())] {
        for line in tail_text(bytes, OUTPUT_TAIL)
            .lines()
            .filter(|line| !line.is_empty())
        {
            eprintln!("pd2-launcher:   {stream}: {}", redacted(line));
        }
    }
    record(ActivityEvent::Process {
        label: label.to_owned(),
        outcome: outcome.to_owned(),
        exit_code,
        stdout: tail_text(output.stdout(), OUTPUT_TAIL),
        stderr: tail_text(output.stderr(), OUTPUT_TAIL),
    });
}

/// A failed download attempt, by host only.
pub(crate) fn record_download(target: &str, url: &str, status: Option<u16>, error: &str) {
    record(ActivityEvent::Download {
        target: target.to_owned(),
        host: crate::sources::redacted_host(url),
        status,
        error: error.to_owned(),
    });
}

/// Classify a transport error without its message (which names the URL). A
/// connection that failed says why when its causes tell: `connection-blocked`
/// (the system refused this app the network), `dns-failed`,
/// `connection-refused`, `network-unreachable`; else `connection-failed`.
/// A phone's per-app block mostly drops packets instead of refusing them, so
/// it shows here as `timed-out` or `dns-failed`: the app's network check
/// (`files/logs/network.log` on Android) is what tells a block.
pub(crate) fn transport_error_kind(error: &zed_reqwest::Error) -> &'static str {
    if error.is_timeout() {
        "timed-out"
    } else if error.is_connect() {
        connect_failure_kind(error)
    } else if error.is_redirect() {
        "redirect-refused"
    } else if error.is_body() || error.is_decode() {
        "response-interrupted"
    } else if error.is_request() {
        "request-failed"
    } else {
        "transport-failed"
    }
}

/// Why a connection failed, from the errors that caused it: their I/O kinds,
/// and the resolver's own word for a failed lookup (its messages name no URL).
fn connect_failure_kind(error: &(dyn std::error::Error + 'static)) -> &'static str {
    let mut source = error.source();
    let (mut blocked, mut lookup, mut refused, mut unreachable) = (false, false, false, false);
    while let Some(cause) = source {
        if let Some(io) = cause.downcast_ref::<std::io::Error>() {
            match io.kind() {
                std::io::ErrorKind::PermissionDenied => blocked = true,
                std::io::ErrorKind::ConnectionRefused => refused = true,
                std::io::ErrorKind::NetworkUnreachable | std::io::ErrorKind::HostUnreachable => {
                    unreachable = true;
                }
                _ => {}
            }
        }
        lookup |= cause.to_string().contains("dns error");
        source = cause.source();
    }
    // A block first; a failed lookup over the I/O kind its error carries.
    match (blocked, lookup, refused, unreachable) {
        (true, ..) => "connection-blocked",
        (_, true, ..) => "dns-failed",
        (_, _, true, _) => "connection-refused",
        (_, _, _, true) => "network-unreachable",
        _ => "connection-failed",
    }
}

/// Every retained entry, oldest first.
pub(crate) fn read(directory: &Path) -> Vec<ActivityEntry> {
    [PREVIOUS_FILE_NAME, FILE_NAME]
        .into_iter()
        .flat_map(|name| read_file(&directory.join(name)))
        .collect()
}

fn read_file(path: &Path) -> Vec<ActivityEntry> {
    let Ok(bytes) = read_bounded_regular(path, 2 * FILE_LIMIT as u64) else {
        return Vec::new();
    };
    String::from_utf8_lossy(&bytes)
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

/// Append one entry under `directory`, rotating a full file.
pub(crate) fn append(directory: &Path, entry: ActivityEntry) -> io::Result<()> {
    Directory::ensure_absolute(directory)?;
    let path = directory.join(FILE_NAME);
    let mut entries = read_file(&path);
    match entries.last_mut() {
        Some(last)
            if last.job == entry.job
                && last.event == entry.event
                && matches!(entry.event, ActivityEvent::Download { .. }) =>
        {
            last.repeat = last.repeat.saturating_add(1);
            last.at = entry.at;
        }
        _ => entries.push(entry),
    }
    let encoded = encode(&entries)?;
    if encoded.len() <= FILE_LIMIT {
        return atomic_write(&path, &encoded);
    }
    // Full: the older entries become the previous file.
    let latest = entries.pop().expect("an entry was just appended");
    atomic_write(&directory.join(PREVIOUS_FILE_NAME), &encode(&entries)?)?;
    atomic_write(&path, &encode(&[latest])?)
}

fn encode(entries: &[ActivityEntry]) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    for entry in entries {
        serde_json::to_writer(&mut bytes, entry).map_err(io::Error::other)?;
        bytes.push(b'\n');
    }
    Ok(bytes)
}

fn redact_event(event: ActivityEvent, redactor: &Redactor) -> ActivityEvent {
    let text = |value: String| bounded(&redactor.text(&value), TEXT_LIMIT);
    match event {
        ActivityEvent::JobFailed { code } => ActivityEvent::JobFailed { code: text(code) },
        ActivityEvent::StageFailed { code, detail } => ActivityEvent::StageFailed {
            code: text(code),
            detail: text(detail),
        },
        ActivityEvent::Process {
            label,
            outcome,
            exit_code,
            stdout,
            stderr,
        } => ActivityEvent::Process {
            label: text(label),
            outcome: text(outcome),
            exit_code,
            stdout: redactor.text(&stdout),
            stderr: redactor.text(&stderr),
        },
        ActivityEvent::PayloadRejected {
            payload,
            file,
            check,
            size,
        } => ActivityEvent::PayloadRejected {
            payload: text(payload),
            file: text(file),
            check: text(check),
            size,
        },
        ActivityEvent::Download {
            target,
            host,
            status,
            error,
        } => ActivityEvent::Download {
            target: text(target),
            host: text(host),
            status,
            error: text(error),
        },
        other => other,
    }
}

/// The last `limit` bytes of `bytes`, as text.
pub(crate) fn tail_text(bytes: &[u8], limit: usize) -> String {
    let start = bytes.len().saturating_sub(limit);
    let text = String::from_utf8_lossy(&bytes[start..]).into_owned();
    if start > 0 {
        format!("…{text}")
    } else {
        text
    }
}

fn bounded(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_owned();
    }
    let mut end = limit;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

pub(crate) fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

/// A short, key-free name for a process: `wineserver -w`, `wine
/// Installer.exe`, or the executable's own name. Arguments that are options
/// (such as the installer's `--key1=`) are never used.
pub(crate) fn process_label(executable: &Path, arguments: &[std::ffi::OsString]) -> String {
    let name = |value: &OsStr| {
        let value = value.to_string_lossy();
        let base = value
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or_default()
            .to_owned();
        bounded(&base, 64)
    };
    let program = arguments
        .iter()
        .filter(|argument| {
            let argument = argument.to_string_lossy();
            !argument.starts_with('-') && !argument.ends_with("/wine")
        })
        .map(|argument| name(argument))
        .next();
    let executable_name = name(executable.as_os_str());
    match program {
        Some(program) if executable_name.chars().all(|c| c.is_ascii_digit()) => {
            format!("wine {program}")
        }
        _ if executable_name.chars().all(|c| c.is_ascii_digit()) => {
            let options = arguments
                .iter()
                .map(|argument| argument.to_string_lossy())
                .filter(|argument| matches!(argument.as_ref(), "-k" | "-w"))
                .collect::<Vec<_>>()
                .join(" ");
            format!("wineserver {options}").trim_end().to_owned()
        }
        _ => executable_name,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An error caused by `causes`, innermost last, as a connector reports it.
    #[derive(Debug)]
    struct Chain(String, Option<Box<dyn std::error::Error + 'static>>);

    impl std::fmt::Display for Chain {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str(&self.0)
        }
    }

    impl std::error::Error for Chain {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            self.1.as_deref()
        }
    }

    fn failed_by(cause: Box<dyn std::error::Error + 'static>) -> Chain {
        Chain(
            "error sending request".to_owned(),
            Some(Box::new(Chain(
                "client error (Connect)".to_owned(),
                Some(cause),
            ))),
        )
    }

    #[test]
    fn a_failed_connection_says_why_when_its_causes_tell() {
        let io = |kind| -> Box<dyn std::error::Error> { Box::new(std::io::Error::from(kind)) };
        assert_eq!(
            connect_failure_kind(&failed_by(io(std::io::ErrorKind::PermissionDenied))),
            "connection-blocked"
        );
        assert_eq!(
            connect_failure_kind(&failed_by(io(std::io::ErrorKind::ConnectionRefused))),
            "connection-refused"
        );
        assert_eq!(
            connect_failure_kind(&failed_by(io(std::io::ErrorKind::NetworkUnreachable))),
            "network-unreachable"
        );
        let lookup = Box::new(Chain(
            "dns error".to_owned(),
            Some(Box::new(std::io::Error::other(
                "failed to lookup address information",
            ))),
        ));
        assert_eq!(connect_failure_kind(&failed_by(lookup)), "dns-failed");
        assert_eq!(
            connect_failure_kind(&failed_by(io(std::io::ErrorKind::HostUnreachable))),
            "network-unreachable"
        );
        // A lookup that failed with a refused query is still a failed lookup.
        let refused_lookup = Box::new(Chain(
            "dns error".to_owned(),
            Some(io(std::io::ErrorKind::ConnectionRefused)),
        ));
        assert_eq!(
            connect_failure_kind(&failed_by(refused_lookup)),
            "dns-failed"
        );
        // A block wins over the failed lookup it caused.
        let blocked_lookup = Box::new(Chain(
            "dns error".to_owned(),
            Some(io(std::io::ErrorKind::PermissionDenied)),
        ));
        assert_eq!(
            connect_failure_kind(&failed_by(blocked_lookup)),
            "connection-blocked"
        );
        assert_eq!(
            connect_failure_kind(&failed_by(io(std::io::ErrorKind::Other))),
            "connection-failed"
        );
    }
    use tempfile::TempDir;

    fn download(host: &str) -> ActivityEntry {
        ActivityEntry {
            at: 1,
            job: Some(3),
            event: ActivityEvent::Download {
                target: "wine".to_owned(),
                host: host.to_owned(),
                status: Some(503),
                error: "status".to_owned(),
            },
            repeat: 1,
        }
    }

    #[test]
    fn winsock_lines_keep_connects_closes_lookups_and_errors_only() {
        assert_eq!(
            winsock_line(
                "123.456:0020:0024:trace:winsock:connect socket 0x1a4, addr { family AF_INET, \
                 address 198.51.100.7, port 6112 }, len 16"
            ),
            WinsockLine::Kept(
                "0020:0024 connect socket 0x1a4, addr { family AF_INET, address 198.51.100.7, \
                 port 6112 }, len 16"
                    .to_owned()
            )
        );
        assert_eq!(
            winsock_line("0024:trace:winsock:closesocket 0x1a4"),
            WinsockLine::Kept("0024 closesocket 0x1a4".to_owned())
        );
        assert_eq!(
            winsock_line("0020:0024:warn:winsock:WS2_recv_base unsupported flags 0x10"),
            WinsockLine::Kept("0020:0024 warn WS2_recv_base unsupported flags 0x10".to_owned())
        );
        assert_eq!(
            winsock_line(
                "123.456:0020:0024:trace:winsock:WS2_recv_base socket 0x1a4, buffers 0x22f000, \
                 buffer_count 1, flags 0"
            ),
            WinsockLine::Dropped
        );
        assert_eq!(
            winsock_line("0020:0024:trace:winsock:select read 0x22f100, timeout 0x0"),
            WinsockLine::Dropped
        );
        assert_eq!(
            winsock_line("0020:0024:trace:seh:dispatch_exception code=c0000005"),
            WinsockLine::Other
        );
        assert_eq!(winsock_line("[BOX64] Using Box64"), WinsockLine::Other);
    }

    #[test]
    fn a_socket_close_tells_its_sends_and_answers() {
        let mut tally = SocketTally::default();
        let t = 1_792_886_400_000;
        let connect = "1.0:0020:0154:trace:winsock:connect socket 0x5c0, addr { family AF_INET, \
                       address 203.0.113.4, port 4000 }, len 16";
        assert_eq!(tally.line(t, connect), None);
        for (at, line) in [
            (
                t + 10,
                "0020:0154:trace:winsock:WS2_sendto socket 0x5c0, buffers 0x1, buffer_count 1, flags 0",
            ),
            (t + 11, "0020:0154:trace:winsock:WS2_sendto status 0."),
            (
                t + 20,
                "0020:0158:trace:winsock:WS2_recv_base socket 0x5c0, buffers 0x2, buffer_count 1, flags 0",
            ),
            (t + 250, "0020:0158:trace:winsock:WS2_recv_base status 0."),
            (
                t + 260,
                "0020:0158:trace:winsock:WS2_recv_base socket 0x5c0, buffers 0x2, buffer_count 1, flags 0",
            ),
            (t + 900, "0020:0158:trace:winsock:WS2_recv_base status 0."),
            // Another socket and another thread change nothing here.
            (
                t + 950,
                "0020:0160:trace:winsock:WS2_recv_base socket 0x600, buffers 0x2, buffer_count 1, flags 0",
            ),
            (t + 951, "0020:0160:trace:winsock:WS2_recv_base status 0."),
        ] {
            assert_eq!(tally.line(at, line), None);
        }
        assert_eq!(
            tally.line(t + 15_070, "0020:0024:trace:winsock:closesocket 0x5c0"),
            Some(
                "0020:0024 socket 0x5c0 to 203.0.113.4:4000 closed after 15.07 s: 1 sends, 2 answers \
                 (first after 0.25 s, last after 0.90 s)"
                    .to_owned()
            )
        );
        // The close ends the socket: a second close tells nothing.
        assert_eq!(
            tally.line(t + 15_100, "0020:0024:trace:winsock:closesocket 0x5c0"),
            None
        );
    }

    #[test]
    fn a_socket_with_no_answer_and_a_failed_receive_says_so() {
        let mut tally = SocketTally::default();
        let t = 1_792_886_400_000;
        let connect = "0020:0154:trace:winsock:connect socket 0x5d4, addr { family AF_INET, \
                       address 203.0.113.5, port 4000 }, len 16";
        assert_eq!(tally.line(t, connect), None);
        assert_eq!(
            tally.line(t + 5, "0020:0154:trace:winsock:WS2_recv_base socket 0x5d4, buffers 0x2, buffer_count 1, flags 0"),
            None
        );
        assert_eq!(
            tally.line(
                t + 15_000,
                "0020:0154:trace:winsock:WS2_recv_base status 0xc0000120."
            ),
            None
        );
        assert_eq!(
            tally.line(t + 15_061, "0020:0024:trace:winsock:closesocket 0x5d4"),
            Some(
                "0020:0024 socket 0x5d4 to 203.0.113.5:4000 closed after 15.06 s: 0 sends, 0 answers, \
                 1 calls failed"
                    .to_owned()
            )
        );
        // A close of a socket with no connect (a listener, a lookup) tells nothing.
        assert_eq!(
            tally.line(t, "0020:0024:trace:winsock:closesocket 0x99"),
            None
        );
    }

    #[test]
    fn network_lines_are_limited_each_second_and_the_count_names_its_second() {
        let start = 1_792_886_400_000;
        let mut lines = NetworkLines::default();
        let written = (0..25)
            .flat_map(|index| lines.event(start + index, "connect x"))
            .collect::<Vec<_>>();
        assert_eq!(written.len(), 20, "{written:?}");
        // A minute later, the count of that second comes first, with its time.
        let later = lines.event(start + 61_000, "closesocket 0x1a4");
        assert_eq!(
            later,
            [
                format!(
                    "pd2-launcher: network: 5 more lines in the second from {} were left out",
                    crate::report::utc_time_millis(start)
                ),
                format!(
                    "pd2-launcher: network {} closesocket 0x1a4",
                    crate::report::utc_time_millis(start + 61_000)
                ),
            ]
        );
        // A count with no network line after it: when its second is over (at
        // another program line), or when a program's output ends.
        for index in 1..22 {
            lines.event(start + 61_000 + index, "connect y");
        }
        assert!(lines.second_passed(start + 61_900).is_empty());
        let over = lines.second_passed(start + 62_000);
        assert_eq!(over.len(), 1);
        assert!(
            over[0].contains(": 2 more lines in the second from"),
            "{over:?}"
        );
        assert_eq!(lines.flush(), None);
        lines.event(start + 63_000, "connect z");
        for index in 1..21 {
            lines.event(start + 63_000 + index, "connect z");
        }
        assert!(
            lines
                .flush()
                .is_some_and(|line| line.contains(": 1 more lines"))
        );
    }

    /// A Winsock line of Wine's channel, as Detailed logging gets it.
    fn wine(tid: &str, function: &str, words: &str) -> String {
        format!("123.456:0020:{tid}:trace:winsock:{function} {words}")
    }

    fn game_server_connect() -> String {
        wine(
            "0154",
            "connect",
            "socket 0x5c0, addr { family AF_INET, address 203.0.113.4, port 4000 }, len 16",
        )
    }

    fn realm_connect() -> String {
        wine(
            "0154",
            "connect",
            "socket 0x5c8, addr { family AF_INET, address 198.51.100.7, port 6113 }, len 16",
        )
    }

    fn as_is(line: &str) -> String {
        line.to_owned()
    }

    #[test]
    fn a_game_server_connect_writes_the_ten_seconds_before_and_then_every_winsock_line() {
        let t = 1_792_886_400_000;
        let utc = crate::report::utc_time_millis;
        let mut lines = NetworkLines::default();
        let select = wine("0158", "select", "read 0x22f100, timeout 0x0");
        let ioctl = wine("0158", "sock_ioctl", "handle 0x5a4, code 0x12024");
        // Before the window: an old wait (more than 10 s before), then waits,
        // a receive and a kept lookup.
        assert!(lines.winsock(t - 12_000, &select, as_is).is_empty());
        assert!(lines.winsock(t - 3_500, &select, as_is).is_empty());
        assert!(lines.winsock(t - 3_400, &ioctl, as_is).is_empty());
        assert!(lines.winsock(t - 3_300, &select, as_is).is_empty());
        assert!(
            lines
                .winsock(
                    t - 3_000,
                    &wine(
                        "0158",
                        "WS2_recv_base",
                        "socket 0x4a0, buffers 0x2, buffer_count 1, flags 0"
                    ),
                    as_is
                )
                .is_empty()
        );
        assert_eq!(
            lines.winsock(t - 2_500, &wine("0154", "getaddrinfo", "node \"x\""), as_is),
            [format!(
                "pd2-launcher: network {} 0020:0154 getaddrinfo node \"x\"",
                utc(t - 2_500)
            )]
        );
        assert!(lines.winsock(t - 400, &select, as_is).is_empty());
        let opened = lines.winsock(t, &game_server_connect(), as_is);
        assert_eq!(
            opened,
            [
                format!(
                    "pd2-launcher: network window: opened at {} by 0020:0154 connect to 203.0.113.4:4000: \
                     the 6 Winsock lines of the 10 s before, then every Winsock line for 20 s (select and \
                     sock_ioctl counted each second)",
                    utc(t)
                ),
                format!(
                    "pd2-launcher: network window: select ×2, sock_ioctl ×1 in the second from {}",
                    utc(t - 4_000)
                ),
                "pd2-launcher: network window -3.000 s 0020:0158 WS2_recv_base socket 0x4a0, buffers 0x2, \
                 buffer_count 1, flags 0"
                    .to_owned(),
                "pd2-launcher: network window -2.500 s 0020:0154 getaddrinfo node \"x\"".to_owned(),
                format!(
                    "pd2-launcher: network window: select ×1 in the second from {}",
                    utc(t - 1_000)
                ),
                "pd2-launcher: network window +0.000 s 0020:0154 connect socket 0x5c0, addr { family \
                 AF_INET, address 203.0.113.4, port 4000 }, len 16"
                    .to_owned(),
            ]
        );
        // In the window: every send and receive, with no limit a second; the
        // waits as one count a second.
        let mut written = Vec::new();
        for index in 0..100 {
            written.extend(lines.winsock(
                t + 1_000 + index,
                &wine(
                    "0154",
                    "WS2_sendto",
                    "socket 0x5c0, buffers 0x1, buffer_count 1, flags 0",
                ),
                as_is,
            ));
            written.extend(lines.winsock(t + 1_000 + index, &select, as_is));
            written.extend(lines.winsock(t + 1_000 + index, &ioctl, as_is));
        }
        assert_eq!(written.len(), 100);
        assert_eq!(
            written[0],
            "pd2-launcher: network window +1.000 s 0020:0154 WS2_sendto socket 0x5c0, buffers 0x1, \
             buffer_count 1, flags 0"
        );
        // A warning of a wait is written as it is.
        assert_eq!(
            lines.winsock(t + 1_500, "0020:0158:warn:winsock:select bad socket", as_is),
            ["pd2-launcher: network window +1.500 s 0020:0158 warn select bad socket".to_owned()]
        );
        // The next second's first line comes after the count of the second before.
        assert_eq!(
            lines.winsock(t + 15_050, &wine("0154", "WS2_sendto", "status 0."), as_is),
            [
                format!(
                    "pd2-launcher: network window: select ×100, sock_ioctl ×100 in the second from {}",
                    utc(t + 1_000)
                ),
                "pd2-launcher: network window +15.050 s 0020:0154 WS2_sendto status 0.".to_owned(),
            ]
        );
        lines.winsock(t + 19_000, &select, as_is);
        // The end of the window: the count of its last second, its last line,
        // then the lines as before.
        let after = lines.winsock(t + 20_000, &select, as_is);
        assert_eq!(
            after,
            [
                format!(
                    "pd2-launcher: network window: select ×1 in the second from {}",
                    utc(t + 19_000)
                ),
                format!(
                    "pd2-launcher: network window: closed 20.0 s after its open at {}: 103 Winsock lines and \
                     201 select and sock_ioctl calls in it",
                    utc(t)
                ),
            ]
        );
        assert!(!lines.window.is_open());
        // Outside the window, the kept lines again at most 20 a second.
        let kept = (0..25)
            .flat_map(|index| {
                lines.winsock(
                    t + 30_000 + index,
                    &wine("0154", "closesocket", "0x5c0"),
                    as_is,
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(kept.len(), 20, "{kept:?}");
        // The lines of the window are not written again by the next window.
        let next = lines.winsock(
            t + 32_000,
            &wine(
                "0154",
                "connect",
                "socket 0x5d0, addr { family AF_INET, address 198.51.100.7, port 6113 }, len 16",
            ),
            as_is,
        );
        assert!(
            next[0].contains("by 0020:0154 connect to 198.51.100.7:6113: the 25 Winsock lines"),
            "{next:?}"
        );
        assert!(
            next.iter().all(|line| !line.contains("WS2_sendto")),
            "{next:?}"
        );
    }

    #[test]
    fn a_realm_connect_in_an_open_window_moves_its_end_but_not_past_thirty_seconds() {
        let t = 1_792_886_400_000;
        let mut lines = NetworkLines::default();
        // A connect to another port (the official launcher's HTTPS) opens no window.
        let https = wine(
            "0154",
            "connect",
            "socket 0x1a4, addr { family AF_INET, address 198.51.100.7, port 443 }, len 16",
        );
        assert_eq!(lines.winsock(t - 1_000, &https, as_is).len(), 1);
        assert!(!lines.window.is_open());
        lines.winsock(t, &realm_connect(), as_is);
        // A realm connect 5 s after: the end moves to 25 s.
        assert_eq!(
            lines.winsock(t + 5_000, &realm_connect(), as_is)[0],
            "pd2-launcher: network window +5.000 s: 0020:0154 connect to 198.51.100.7:6113: the window \
             ends at +25.000 s (at most 30 s after its first connect)"
        );
        // A realm connect 17 s after: the end stops at 30 s, not 37 s.
        assert_eq!(
            lines.winsock(t + 17_000, &realm_connect(), as_is)[0],
            "pd2-launcher: network window +17.000 s: 0020:0154 connect to 198.51.100.7:6113: the window \
             ends at +30.000 s (at most 30 s after its first connect)"
        );
        assert_eq!(
            lines.winsock(t + 29_999, &wine("0154", "closesocket", "0x5c8"), as_is),
            ["pd2-launcher: network window +29.999 s 0020:0154 closesocket 0x5c8".to_owned()]
        );
        // The end also comes with no Winsock line: at a program's other line
        // or when its output ends.
        assert!(lines.window.passed(t + 29_999).is_empty());
        let closed = lines.window.passed(t + 30_000);
        assert_eq!(closed.len(), 1);
        assert!(
            closed[0].contains("closed 30.0 s after its open at")
                && closed[0].ends_with(": 4 Winsock lines and 0 select and sock_ioctl calls in it"),
            "{closed:?}"
        );
        assert!(lines.window.close().is_empty());
    }

    #[test]
    fn a_game_server_connect_in_a_realm_window_keeps_its_twenty_seconds() {
        let t = 1_792_886_400_000;
        let mut lines = NetworkLines::default();
        lines.winsock(t, &realm_connect(), as_is);
        // The window's first game-server connect, 18 s after the realm
        // connect: its full 20 s, to 38 s (not cut at 30 s).
        assert_eq!(
            lines.winsock(t + 18_000, &game_server_connect(), as_is)[0],
            "pd2-launcher: network window +18.000 s: 0020:0154 connect to 203.0.113.4:4000: the window \
             ends at +38.000 s (its first game-server connect: its full 20 s)"
        );
        // A realm connect after it does not shorten it.
        assert!(
            lines.winsock(t + 20_000, &realm_connect(), as_is)[0]
                .ends_with("the window ends at +38.000 s (at most 30 s after its first connect)")
        );
        assert!(lines.window.passed(t + 37_999).is_empty());
        // A second game-server connect: at most 45 s after the first one (18 + 45 = 63).
        assert!(
            lines.winsock(t + 30_000, &game_server_connect(), as_is)[0].ends_with(
                "the window ends at +50.000 s (at most 45 s after its first game-server connect)"
            )
        );
        assert!(
            lines.winsock(t + 49_000, &game_server_connect(), as_is)[0].ends_with(
                "the window ends at +63.000 s (at most 45 s after its first game-server connect)"
            )
        );
        assert_eq!(
            lines.winsock(t + 62_999, &wine("0154", "closesocket", "0x5c0"), as_is),
            ["pd2-launcher: network window +62.999 s 0020:0154 closesocket 0x5c0".to_owned()]
        );
        let closed = lines.window.passed(t + 63_000);
        assert!(
            closed[0].contains("closed 63.0 s after its open at"),
            "{closed:?}"
        );
        assert!(!lines.window.is_open());
    }

    #[test]
    fn a_game_server_connect_after_two_realm_connects_still_gets_its_twenty_seconds() {
        // The review's case: realm at +0 s, realm at +17 s (end +30 s), game
        // server at +28 s: its end is +48 s, not +45 s.
        let t = 1_792_886_400_000;
        let mut lines = NetworkLines::default();
        lines.winsock(t, &realm_connect(), as_is);
        assert!(
            lines.winsock(t + 17_000, &realm_connect(), as_is)[0]
                .ends_with("the window ends at +30.000 s (at most 30 s after its first connect)")
        );
        assert!(
            lines.winsock(t + 28_000, &game_server_connect(), as_is)[0].ends_with(
                "the window ends at +48.000 s (its first game-server connect: its full 20 s)"
            )
        );
        assert!(lines.window.passed(t + 47_999).is_empty());
        assert_eq!(lines.window.passed(t + 48_000).len(), 1);
    }

    #[test]
    fn the_end_of_the_output_that_opened_a_window_closes_it_and_no_other_does() {
        let t = 1_792_886_400_000;
        let mut lines = NetworkLines::default();
        lines.winsock(t, &game_server_connect(), as_is);
        lines.winsock(t + 3_100, &wine("0158", "select", "x"), as_is);
        lines.winsock(t + 3_200, &wine("0158", "sock_ioctl", "y"), as_is);
        // Another program's output ends: the window stays open.
        let other = std::thread::spawn(|| std::thread::current().id())
            .join()
            .unwrap();
        assert!(lines.window.ended(t + 5_000, other).is_empty());
        assert!(lines.window.is_open());
        // Its own program's output ends: the last second's count, then its
        // end with the time it was really open.
        assert_eq!(
            lines.window.ended(t + 7_250, std::thread::current().id()),
            [
                format!(
                    "pd2-launcher: network window: select ×1, sock_ioctl ×1 in the second from {}",
                    crate::report::utc_time_millis(t + 3_000)
                ),
                format!(
                    "pd2-launcher: network window: closed 7.2 s after its open at {} (its program's output \
                     ended): 1 Winsock lines and 2 select and sock_ioctl calls in it",
                    crate::report::utc_time_millis(t)
                ),
            ]
        );
        assert!(!lines.window.is_open());
        // After its end has passed, any output's end writes its last line.
        lines.winsock(t + 10_000, &game_server_connect(), as_is);
        assert_eq!(lines.window.ended(t + 30_000, other).len(), 1);
    }

    #[test]
    fn window_lines_are_redacted_before_their_cut_and_headers_too() {
        let t = 1_792_886_400_000;
        let mut lines = NetworkLines::default();
        let hide = |text: &str| {
            text.replace("203.0.113.4", "<server>")
                .replace(&"x".repeat(400), "X")
        };
        let opened = lines.winsock(t, &game_server_connect(), hide);
        assert!(opened[0].contains("connect to <server>:4000"), "{opened:?}");
        assert!(
            opened[1].contains("address <server>, port 4000"),
            "{opened:?}"
        );
        // A text over 300 bytes that the redaction makes short is not cut.
        let long = lines.winsock(
            t + 10,
            &wine(
                "0154",
                "getaddrinfo",
                &format!("node \"{}\" end", "x".repeat(400)),
            ),
            hide,
        );
        assert!(long[0].ends_with("getaddrinfo node \"X\" end"), "{long:?}");
        // A text that stays long is cut after the redaction.
        let cut = lines.winsock(t + 20, &wine("0154", "getaddrinfo", &"y".repeat(400)), hide);
        assert!(cut[0].len() < 400, "{}", cut[0].len());
        // All lines of a step go out as one text, in their order.
        assert_eq!(
            render_all(
                vec![
                    NetLine::Ready("a".to_owned()),
                    NetLine::Redact {
                        head: "b ".to_owned(),
                        text: "203.0.113.4".to_owned(),
                        limit: None
                    },
                ],
                &hide
            ),
            "a\nb <server>\n"
        );
    }

    #[test]
    fn after_its_end_the_next_connect_opens_a_new_window_with_its_own_lines_before() {
        let t = 1_792_886_400_000;
        let mut lines = NetworkLines::default();
        lines.winsock(t, &realm_connect(), as_is);
        lines.winsock(t + 17_000, &realm_connect(), as_is);
        // After the 30 s end: the lines wait for the next window again.
        assert_eq!(
            lines
                .winsock(t + 31_000, &wine("0158", "select", "x"), as_is)
                .len(),
            1,
            "only the window's last line"
        );
        lines.winsock(
            t + 32_000,
            &wine(
                "0158",
                "WS2_recv_base",
                "socket 0x5c0, buffers 0x2, buffer_count 1, flags 0",
            ),
            as_is,
        );
        let opened = lines.winsock(t + 33_000, &game_server_connect(), as_is);
        assert_eq!(
            opened[0],
            format!(
                "pd2-launcher: network window: opened at {} by 0020:0154 connect to 203.0.113.4:4000: \
                 the 2 Winsock lines of the 10 s before, then every Winsock line for 20 s (select and \
                 sock_ioctl counted each second)",
                crate::report::utc_time_millis(t + 33_000)
            )
        );
        assert_eq!(
            opened[1..],
            [
                format!(
                    "pd2-launcher: network window: select ×1 in the second from {}",
                    crate::report::utc_time_millis(t + 31_000)
                ),
                "pd2-launcher: network window -1.000 s 0020:0158 WS2_recv_base socket 0x5c0, buffers 0x2, \
                 buffer_count 1, flags 0"
                    .to_owned(),
                "pd2-launcher: network window +0.000 s 0020:0154 connect socket 0x5c0, addr { family \
                 AF_INET, address 203.0.113.4, port 4000 }, len 16"
                    .to_owned(),
            ]
        );
        // Its own end: 20 s after its connect.
        assert!(lines.window.passed(t + 52_999).is_empty());
        assert_eq!(lines.window.passed(t + 53_000).len(), 1);
    }

    #[test]
    fn the_window_keeps_at_most_its_bound_of_lines_before() {
        let t = 1_792_886_400_000;
        let mut lines = NetworkLines::default();
        for index in 0..(WINDOW_BEFORE_LINES as u64 + 5) {
            lines.winsock(
                t - 5_000 + index / 10,
                &wine("0158", "WS2_recv_base", "status 0."),
                as_is,
            );
        }
        let opened = lines.winsock(t, &game_server_connect(), as_is);
        assert_eq!(opened.len(), WINDOW_BEFORE_LINES + 2);
    }

    #[test]
    fn window_lines_are_redacted_and_never_hold_data() {
        let t = 1_792_886_400_000;
        let mut lines = NetworkLines::default();
        let home = std::env::var("HOME").unwrap_or_default();
        lines.winsock(
            t - 100,
            &wine(
                "0154",
                "getaddrinfo",
                &format!("node \"{home}/x\" --key1=ABCDEFGHJKLMNPQRSTVWXYZ234"),
            ),
            redacted,
        );
        let mut out = lines.winsock(t, &game_server_connect(), redacted);
        out.extend(lines.winsock(
            t + 10,
            &wine(
                "0158",
                "WS2_recv_base",
                &format!("socket 0x5c0 {home}/y --key1=ABCDEFGHJKLMNPQRSTVWXYZ234"),
            ),
            redacted,
        ));
        assert_eq!(out.len(), 4, "{out:?}");
        for line in &out {
            assert!(!line.contains("ABCDEFGHJKLMNPQRSTVWXYZ234"), "{line}");
            if !home.is_empty() && home != "/" {
                assert!(!line.contains(&home), "{line}");
            }
        }
    }

    #[test]
    fn the_capture_keeps_all_but_wine_noise_and_network_lines_twenty_a_second() {
        assert_eq!(
            capture_line(b"0020:0024:trace:loaddll:build_module Loaded L\"ntdll.dll\": builtin"),
            CaptureLine::Drop
        );
        assert_eq!(
            capture_line(b"0020:0024:trace:winsock:select read 0x22f100"),
            CaptureLine::Drop
        );
        assert_eq!(
            capture_line(b"0020:0024:trace:winsock:connect socket 0x1a4"),
            CaptureLine::Network
        );
        assert_eq!(
            capture_line(b"0020:0024:warn:winsock:WS2_recv_base unsupported flags"),
            CaptureLine::Network
        );
        assert_eq!(
            capture_line(b"err:module:import_dll Library X.dll not found"),
            CaptureLine::Keep
        );
        assert_eq!(capture_line(b""), CaptureLine::Keep);
        let mut network = CaptureNetworkLines::default();
        let taken = (0..25).filter(|_| network.take(1_792_886_400)).count();
        assert_eq!(taken, 20);
        assert!(network.take(1_792_886_401));
    }

    #[test]
    fn detailed_logging_shows_longer_output_lines() {
        assert_eq!(output_line_limit(false), 300);
        assert_eq!(output_line_limit(true), 1000);
        let line = "y".repeat(900);
        assert_eq!(bounded(&line, output_line_limit(true)), line);
        assert!(bounded(&line, output_line_limit(false)).len() < line.len());
    }

    #[test]
    fn live_output_masks_keys_and_the_home_folder() {
        let home = std::env::var("HOME").unwrap_or_default();
        live_output(
            &format!("installer --key1=ABCDEFGHJKLMNPQRSTVWXYZ234 at {home}/.pd2launcher/game"),
            false,
        );
        let shown = recent_lines(RECENT_LINES)
            .into_iter()
            .rfind(|line| line.contains("installer"))
            .unwrap();
        assert!(!shown.contains("ABCDEFGHJKLMNPQRSTVWXYZ234"), "{shown}");
        if home.len() > 1 {
            assert!(!shown.contains(&home), "{shown}");
        }
    }

    #[test]
    fn entries_are_bounded_rotated_and_repeats_coalesce() {
        let fixture = TempDir::new().unwrap();
        let directory = fixture.path().join("diagnostics");
        append(&directory, download("a.example")).unwrap();
        append(&directory, download("a.example")).unwrap();
        let entries = read(&directory);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].repeat, 2);

        let big = "x".repeat(OUTPUT_TAIL);
        for index in 0..200 {
            append(
                &directory,
                ActivityEntry {
                    at: index,
                    job: None,
                    event: ActivityEvent::Process {
                        label: "wine Installer.exe".to_owned(),
                        outcome: "exited".to_owned(),
                        exit_code: Some(1),
                        stdout: big.clone(),
                        stderr: big.clone(),
                    },
                    repeat: 1,
                },
            )
            .unwrap();
        }
        for name in [FILE_NAME, PREVIOUS_FILE_NAME] {
            let metadata = std::fs::metadata(directory.join(name)).unwrap();
            assert!(metadata.len() as usize <= FILE_LIMIT);
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(metadata.permissions().mode() & 0o077, 0);
        }
        let entries = read(&directory);
        assert_eq!(entries.last().unwrap().at, 199);
    }

    #[test]
    fn process_labels_never_carry_option_values() {
        let arguments = [
            "/proc/self/fd/236/wine",
            "/proc/self/fd/235/x/Installer.exe",
            "--key1=SECRET",
        ]
        .map(std::ffi::OsString::from);
        assert_eq!(
            process_label(Path::new("/proc/self/fd/230"), &arguments),
            "wine Installer.exe"
        );
        assert_eq!(
            process_label(Path::new("/proc/self/fd/232"), &["-w".into()]),
            "wineserver -w"
        );
        assert_eq!(
            process_label(Path::new("/usr/bin/true"), &["--key1=SECRET".into()]),
            "true"
        );
    }
}
