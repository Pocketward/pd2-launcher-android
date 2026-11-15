//! Fetching payloads one piece at a time: `GET <base>/<index>` with the
//! access cookie, each piece checked against its length and SHA-1 and
//! written across file borders into the preallocated files. Diablo II and
//! Lord of Destruction download at the same time from one pool of
//! connections, whose size [`super::connections`] decides.
//!
//! Translated from blizzard-legacy-dl's `src/installer.zig` (MIT, Copyright
//! (c) 2026 jaenster; `LICENSE` beside this file), with its reliability
//! rules: a time limit per piece and an idle limit per read, a fresh
//! connection and a growing pause after a failed try, a cache-buster query
//! on a retry, every broken body a retry, pooled connections dropped after
//! 30 s idle, further rounds for the pieces that failed while each round
//! gets some further, and a product given up after 8 failed pieces with
//! none fetched.

use std::{
    collections::VecDeque,
    fs::File,
    hash::BuildHasher,
    io::{self, Read},
    os::unix::fs::FileExt,
    sync::{
        Condvar, Mutex, PoisonError,
        atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant, SystemTime},
};

use zed_reqwest::{
    blocking::Client,
    header::{ACCEPT_ENCODING, COOKIE, HeaderValue, PRAGMA},
    redirect,
};

use super::connections::{Controller, Window};
use super::torrent::{AccessCookie, Torrent};
use crate::operation::OperationControl;

/// Failed pieces of one product, with none fetched, after which it is given
/// up: a wall like that is the host refusing the cookie, not bad luck.
const GIVE_UP_FAILURES: usize = 8;
/// Pieces in a row refused (403) after pieces arrived: the cookie expired
/// during the download.
const EXPIRED_REFUSALS: usize = 8;
/// Redirects one piece request may follow, on the piece host only.
const MAX_REDIRECTS: usize = 3;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Pooled connections idle this long are dropped rather than reused.
const POOL_IDLE: Duration = Duration::from_secs(30);
/// How often the pool's count, progress and end are looked at.
const TICK: Duration = Duration::from_millis(100);
/// Pieces per connection a measuring window needs, so the pieces still in
/// flight at its two ends do not blur the speed.
const WINDOW_PIECES_PER_CONNECTION: u64 = 4;
const USER_AGENT: &str = concat!("pd2-launcher/", env!("CARGO_PKG_VERSION"), " linux");

/// The fetch's limits; tests shorten them.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Limits {
    /// The longest one request for a piece may take, start to end.
    pub(crate) piece: Duration,
    /// The longest a connect and answer, or one read of the body, may wait.
    pub(crate) idle: Duration,
    /// Tries per piece after the first.
    pub(crate) retries: usize,
    /// The pause after a failed try, times the try's number.
    pub(crate) backoff: Duration,
    /// Rounds after the first for the pieces that failed, and the pause
    /// before each, times its number.
    pub(crate) extra_rounds: usize,
    pub(crate) round_pause: Duration,
    /// A measuring window's shortest and longest time, and the time after
    /// a change of the count before the next window starts.
    pub(crate) window: Duration,
    pub(crate) longest_window: Duration,
    pub(crate) settle: Duration,
}

impl Limits {
    pub(crate) const STANDARD: Self = Self {
        piece: Duration::from_secs(60),
        idle: Duration::from_secs(20),
        retries: 3,
        backoff: Duration::from_millis(250),
        extra_rounds: 3,
        round_pause: Duration::from_secs(2),
        window: Duration::from_secs(1),
        longest_window: Duration::from_secs(8),
        settle: Duration::from_millis(500),
    };
}

/// Why one request for a piece gave no piece.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RequestFailure {
    /// An answer other than 200.
    Status(u16),
    /// No connection, or the body broke off.
    Transfer,
    /// The piece's time limit, or the idle limit, passed.
    Timeout,
    /// More bytes than the piece has.
    TooLong,
    Cancelled,
}

/// One request for a piece.
pub(crate) struct PieceRequest<'a> {
    pub(crate) url: &'a str,
    pub(crate) cookie: &'a AccessCookie,
    /// The piece's size: a longer body is refused.
    pub(crate) limit: usize,
    pub(crate) deadline: Instant,
    pub(crate) cancelled: &'a dyn Fn() -> bool,
}

/// Where pieces come from.
pub(crate) trait Transport: Sync {
    /// A session with a pool of its own: a retry after a timeout or a broken
    /// connection gets a new one, so it dials again.
    fn session(&self) -> Option<Box<dyn Session>>;
}

pub(crate) trait Session {
    /// The whole body of a 200 answer.
    fn get(&mut self, request: &PieceRequest<'_>) -> Result<Vec<u8>, RequestFailure>;
}

/// The engine's own HTTP client, one per connection of the pool, for the
/// piece host only. Redirects stay on the piece host, its port 80 and its
/// path.
pub(crate) struct HttpTransport {
    host: &'static str,
    /// `None` only in tests, whose host has another port.
    port: Option<u16>,
    path_prefix: &'static str,
    idle: Duration,
    #[cfg(test)]
    no_proxy: bool,
}

impl HttpTransport {
    pub(crate) fn new(idle: Duration) -> Self {
        Self {
            host: super::PIECE_HOST,
            port: Some(80),
            path_prefix: super::PIECE_PATH_PREFIX,
            idle,
            #[cfg(test)]
            no_proxy: false,
        }
    }
}

impl Transport for HttpTransport {
    fn session(&self) -> Option<Box<dyn Session>> {
        let (host, port, path_prefix) = (self.host, self.port, self.path_prefix);
        let policy = redirect::Policy::custom(move |attempt| {
            if redirect_allowed(attempt.url(), host, port, path_prefix)
                && attempt.previous().len() <= MAX_REDIRECTS
            {
                attempt.follow()
            } else {
                attempt.error("a piece redirect left the piece host")
            }
        });
        let builder = Client::builder()
            .user_agent(USER_AGENT)
            .connect_timeout(CONNECT_TIMEOUT.min(self.idle))
            // An idle bound in the blocking client: connect plus answer,
            // then each read of the body. The piece's own limit is kept
            // between reads.
            .timeout(self.idle)
            .pool_idle_timeout(POOL_IDLE)
            .pool_max_idle_per_host(1)
            .redirect(policy);
        #[cfg(test)]
        let builder = if self.no_proxy {
            builder.no_proxy()
        } else {
            builder
        };
        let client = builder.build().ok()?;
        Some(Box::new(HttpSession { client }))
    }
}

/// Whether a redirect to `url` stays on the piece host: plain HTTP, the
/// host, its port and its path.
fn redirect_allowed(
    url: &zed_reqwest::Url,
    host: &str,
    port: Option<u16>,
    path_prefix: &str,
) -> bool {
    url.scheme() == "http"
        && url.host_str() == Some(host)
        && port.is_none_or(|port| url.port_or_known_default() == Some(port))
        && url.path().starts_with(path_prefix)
}

struct HttpSession {
    client: Client,
}

impl Session for HttpSession {
    fn get(&mut self, request: &PieceRequest<'_>) -> Result<Vec<u8>, RequestFailure> {
        let mut cookie = HeaderValue::from_bytes(request.cookie.header_value())
            .map_err(|_| RequestFailure::Transfer)?;
        cookie.set_sensitive(true);
        let response = self
            .client
            .get(request.url)
            .header(COOKIE, cookie)
            // What Blizzard's downloader sends: WinInet's reload flag.
            .header(PRAGMA, "no-cache")
            .header(ACCEPT_ENCODING, "identity")
            .send()
            .map_err(|error| {
                if error.is_timeout() {
                    RequestFailure::Timeout
                } else {
                    RequestFailure::Transfer
                }
            })?;
        let status = response.status().as_u16();
        if status != 200 {
            return Err(RequestFailure::Status(status));
        }
        if response
            .content_length()
            .is_some_and(|length| length > request.limit as u64)
        {
            return Err(RequestFailure::TooLong);
        }
        read_body(response, request)
    }
}

fn read_body(mut body: impl Read, request: &PieceRequest<'_>) -> Result<Vec<u8>, RequestFailure> {
    let mut data = Vec::with_capacity(request.limit);
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        if (request.cancelled)() {
            return Err(RequestFailure::Cancelled);
        }
        if Instant::now() >= request.deadline {
            return Err(RequestFailure::Timeout);
        }
        let read = match body.read(&mut buffer) {
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) if error.kind() == io::ErrorKind::TimedOut => {
                return Err(RequestFailure::Timeout);
            }
            Err(_) => return Err(RequestFailure::Transfer),
        };
        if read == 0 {
            return Ok(data);
        }
        if data.len() + read > request.limit {
            return Err(RequestFailure::TooLong);
        }
        data.extend_from_slice(&buffer[..read]);
    }
}

/// A piece counted in flight; dropped when its work ends, also by a panic.
struct InFlight<'a>(&'a AtomicUsize);

impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

/// One file of a payload being downloaded.
pub(crate) struct PayloadFile {
    pub(crate) file: File,
    /// Made by this run: it holds no bytes of an earlier one to check.
    pub(crate) fresh: bool,
}

/// One product to download.
pub(crate) struct Download<'a> {
    pub(crate) torrent: &'a Torrent,
    pub(crate) cookie: &'a AccessCookie,
    /// Piece `n` is `<base>/<n>`.
    pub(crate) base: &'a str,
    /// The torrent's files in its order; `None` for a file not kept (the
    /// Mac installer). A piece only in such files is not fetched.
    pub(crate) files: &'a [Option<PayloadFile>],
}

impl Download<'_> {
    fn needed(&self, piece: usize) -> bool {
        self.torrent
            .spans(piece)
            .iter()
            .any(|span| matches!(self.files.get(span.file), Some(Some(_))))
    }

    fn write(&self, piece: usize, data: &[u8]) -> io::Result<()> {
        let mut at = 0;
        for span in self.torrent.spans(piece) {
            let length = span.length as usize;
            if let Some(Some(kept)) = self.files.get(span.file) {
                kept.file
                    .write_all_at(&data[at..at + length], span.offset)?;
            }
            at += length;
        }
        Ok(())
    }

    /// The piece as an earlier run left it, when all its bytes are kept.
    fn read_back(&self, piece: usize) -> Option<Vec<u8>> {
        let mut data = Vec::with_capacity(self.torrent.piece_size(piece) as usize);
        for span in self.torrent.spans(piece) {
            let kept = self.files.get(span.file)?.as_ref()?;
            if kept.fresh {
                return None;
            }
            let start = data.len();
            data.resize(start + span.length as usize, 0);
            kept.file
                .read_exact_at(&mut data[start..], span.offset)
                .ok()?;
        }
        Some(data)
    }
}

/// How one product's download ended.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Outcome {
    /// Every piece it needs is on disk and checked.
    Complete,
    /// Nothing arrived and the host refused (403): the cookie is not
    /// accepted.
    Refused,
    /// Pieces arrived, then the host refused the rest: the cookie expired.
    Expired,
    /// Pieces are still missing after every round: the connection.
    Incomplete,
    /// A piece could not be written (the disk).
    StoreFailed,
}

impl Outcome {
    const fn code(self) -> u8 {
        match self {
            Self::Complete => 1,
            Self::Refused => 2,
            Self::Expired => 3,
            Self::Incomplete => 4,
            Self::StoreFailed => 5,
        }
    }

    const fn from_code(code: u8) -> Option<Self> {
        match code {
            1 => Some(Self::Complete),
            2 => Some(Self::Refused),
            3 => Some(Self::Expired),
            4 => Some(Self::Incomplete),
            5 => Some(Self::StoreFailed),
            _ => None,
        }
    }
}

/// The fetch stopped because the user stopped it; the files are kept.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Cancelled;

/// Bytes of the needed pieces on disk and checked, of all of them.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Progress {
    pub(crate) done: u64,
    pub(crate) total: u64,
}

/// What a whole fetch did.
#[derive(Clone, Debug)]
pub(crate) struct Summary {
    pub(crate) outcomes: Vec<Outcome>,
    /// Bytes fetched, and bytes an earlier run had left that checked.
    pub(crate) fetched: u64,
    pub(crate) resumed: u64,
    pub(crate) elapsed: Duration,
    /// The most connections used at once.
    pub(crate) peak_connections: usize,
}

/// Fetch every needed piece of `downloads` at the same time. Pieces an
/// earlier run left on disk that check are kept. A user's stop keeps the
/// files as they are.
pub(crate) fn fetch(
    downloads: &[Download<'_>],
    transport: &dyn Transport,
    limits: &Limits,
    control: &OperationControl,
    report: &(dyn Fn(Progress) + Sync),
) -> Result<Summary, Cancelled> {
    let started = Instant::now();
    let mut needed = Vec::new();
    let mut total = 0;
    let mut products = Vec::new();
    for (index, download) in downloads.iter().enumerate() {
        let before = needed.len();
        for piece in 0..download.torrent.piece_count() {
            if download.needed(piece) {
                needed.push((index, piece));
                total += download.torrent.piece_size(piece);
            }
        }
        products.push(Product::new(download, needed.len() - before));
    }
    let pool = Pool {
        products: &products,
        transport,
        limits,
        control,
        report,
        total,
        queue: Mutex::new(VecDeque::new()),
        failed: Mutex::new(Vec::new()),
        in_flight: AtomicUsize::new(0),
        target: AtomicUsize::new(0),
        finished: AtomicBool::new(false),
        store_failed: AtomicBool::new(false),
        park: Mutex::new(()),
        wake: Condvar::new(),
        done: AtomicU64::new(0),
        fetched: AtomicU64::new(0),
        fetched_pieces: AtomicU64::new(0),
        requests: AtomicU64::new(0),
        errors: AtomicU64::new(0),
        peak: AtomicUsize::new(0),
    };
    let mut pending = pool.resume(needed)?;
    let resumed = pool.done.load(Ordering::Relaxed);
    let mut random = Random::new();
    random.shuffle(&mut pending);
    let mut controller = Controller::new();
    let mut round = 0;
    let mut before = usize::MAX;
    loop {
        pool.run_round(&mut controller, pending);
        if control.is_cancelled() {
            return Err(Cancelled);
        }
        let mut failed = std::mem::take(&mut *lock(&pool.failed));
        failed.retain(|(product, _)| products[*product].given_up().is_none());
        if failed.is_empty() || pool.store_failed.load(Ordering::Relaxed) {
            break;
        }
        // Not past the round limit, and not when the last round got no
        // piece further: the pieces left are refused, not slow.
        if round >= limits.extra_rounds || failed.len() >= before {
            break;
        }
        eprintln!(
            "pd2-launcher: direct download: {} pieces did not arrive; asking for them again",
            failed.len()
        );
        round += 1;
        if pause(limits.round_pause * round as u32, control) {
            return Err(Cancelled);
        }
        before = failed.len();
        random.shuffle(&mut failed);
        pending = failed;
    }
    let store_failed = pool.store_failed.load(Ordering::Relaxed);
    let outcomes = products
        .iter()
        .map(|product| match product.given_up() {
            Some(outcome) => outcome,
            None if product.done.load(Ordering::Relaxed) == product.needed => Outcome::Complete,
            None if store_failed => Outcome::StoreFailed,
            None => Outcome::Incomplete,
        })
        .collect();
    report(Progress {
        done: pool.done.load(Ordering::Relaxed),
        total,
    });
    Ok(Summary {
        outcomes,
        fetched: pool.fetched.load(Ordering::Relaxed),
        resumed,
        elapsed: started.elapsed(),
        peak_connections: pool.peak.load(Ordering::Relaxed),
    })
}

/// One product's state while the pool works.
struct Product<'a> {
    download: &'a Download<'a>,
    /// Pieces it needs, and of those the ones on disk and checked.
    needed: usize,
    done: AtomicUsize,
    /// Pieces fetched, failed after all their tries, and of those failed
    /// the ones last refused with 403.
    fetched: AtomicUsize,
    failed: AtomicUsize,
    refused: AtomicUsize,
    /// Pieces refused in a row since the last one fetched.
    refused_run: AtomicUsize,
    given_up: AtomicU8,
}

impl<'a> Product<'a> {
    fn new(download: &'a Download<'a>, needed: usize) -> Self {
        Self {
            download,
            needed,
            done: AtomicUsize::new(0),
            fetched: AtomicUsize::new(0),
            failed: AtomicUsize::new(0),
            refused: AtomicUsize::new(0),
            refused_run: AtomicUsize::new(0),
            given_up: AtomicU8::new(0),
        }
    }

    fn given_up(&self) -> Option<Outcome> {
        Outcome::from_code(self.given_up.load(Ordering::Relaxed))
    }

    fn give_up(&self, outcome: Outcome) {
        if self
            .given_up
            .compare_exchange(0, outcome.code(), Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
        {
            eprintln!(
                "pd2-launcher: direct download: {} stopped ({outcome:?}) after {} pieces failed, {} fetched",
                self.download.torrent.name,
                self.failed.load(Ordering::Relaxed),
                self.fetched.load(Ordering::Relaxed)
            );
        }
    }

    fn fetched_one(&self) {
        self.done.fetch_add(1, Ordering::Relaxed);
        self.fetched.fetch_add(1, Ordering::Relaxed);
        self.refused_run.store(0, Ordering::Relaxed);
    }

    /// A piece failed all its tries; `refused` when the last was a 403.
    fn failed_one(&self, refused: bool) {
        let failed = self.failed.fetch_add(1, Ordering::Relaxed) + 1;
        let fetched = self.fetched.load(Ordering::Relaxed);
        if refused {
            let refused = self.refused.fetch_add(1, Ordering::Relaxed) + 1;
            let run = self.refused_run.fetch_add(1, Ordering::Relaxed) + 1;
            if fetched > 0 && run >= EXPIRED_REFUSALS {
                self.give_up(Outcome::Expired);
            }
            if fetched == 0 && failed >= GIVE_UP_FAILURES && refused * 2 >= failed {
                self.give_up(Outcome::Refused);
            }
        } else {
            self.refused_run.store(0, Ordering::Relaxed);
        }
        if fetched == 0 && failed >= GIVE_UP_FAILURES {
            self.give_up(Outcome::Incomplete);
        }
    }
}

struct Pool<'a> {
    products: &'a [Product<'a>],
    transport: &'a dyn Transport,
    limits: &'a Limits,
    control: &'a OperationControl,
    report: &'a (dyn Fn(Progress) + Sync),
    total: u64,
    queue: Mutex<VecDeque<(usize, usize)>>,
    failed: Mutex<Vec<(usize, usize)>>,
    in_flight: AtomicUsize,
    /// The count: worker `n` works while `n` is below it.
    target: AtomicUsize,
    finished: AtomicBool,
    store_failed: AtomicBool,
    park: Mutex<()>,
    wake: Condvar,
    /// Bytes of needed pieces on disk and checked.
    done: AtomicU64,
    /// What the measuring windows read: fetched bytes and pieces, and
    /// requests ended and failed.
    fetched: AtomicU64,
    fetched_pieces: AtomicU64,
    requests: AtomicU64,
    errors: AtomicU64,
    peak: AtomicUsize,
}

/// Where a measuring window started.
#[derive(Clone, Copy)]
struct Mark {
    at: Instant,
    fetched: u64,
    pieces: u64,
    requests: u64,
    errors: u64,
}

impl Pool<'_> {
    fn mark(&self, at: Instant) -> Mark {
        Mark {
            at,
            fetched: self.fetched.load(Ordering::Relaxed),
            pieces: self.fetched_pieces.load(Ordering::Relaxed),
            requests: self.requests.load(Ordering::Relaxed),
            errors: self.errors.load(Ordering::Relaxed),
        }
    }

    /// Check the pieces an earlier run left on disk, several at a time;
    /// the ones still to fetch.
    fn resume(&self, needed: Vec<(usize, usize)>) -> Result<Vec<(usize, usize)>, Cancelled> {
        let next = AtomicUsize::new(0);
        let missing = Mutex::new(Vec::new());
        let threads = thread::available_parallelism().map_or(2, |count| count.get().clamp(1, 8));
        let check = || {
            loop {
                if self.control.is_cancelled() {
                    return;
                }
                let Some(&(product, piece)) = needed.get(next.fetch_add(1, Ordering::Relaxed))
                else {
                    return;
                };
                let download = self.products[product].download;
                match download.read_back(piece) {
                    Some(data) if download.torrent.verify(piece, &data) => {
                        self.done.fetch_add(data.len() as u64, Ordering::Relaxed);
                        self.products[product].done.fetch_add(1, Ordering::Relaxed);
                    }
                    _ => lock(&missing).push((product, piece)),
                }
            }
        };
        thread::scope(|scope| {
            for _ in 1..threads {
                if let Err(error) = thread::Builder::new().spawn_scoped(scope, check) {
                    eprintln!("pd2-launcher: direct download: no thread to check pieces: {error}");
                    break;
                }
            }
            // This thread checks too: a thread the system refuses loses no
            // piece.
            check();
        });
        if self.control.is_cancelled() {
            return Err(Cancelled);
        }
        Ok(missing.into_inner().unwrap_or_else(PoisonError::into_inner))
    }

    /// Fetch `pieces` with the pool, its count decided as it goes; the
    /// pieces that failed are left in `failed`.
    fn run_round(&self, controller: &mut Controller, pieces: Vec<(usize, usize)>) {
        *lock(&self.queue) = pieces.into();
        self.finished.store(false, Ordering::Relaxed);
        thread::scope(|scope| {
            let mut started = 0;
            let mut start = |count: usize| {
                self.target.store(count, Ordering::Relaxed);
                self.peak.fetch_max(count, Ordering::Relaxed);
                while started < count {
                    let slot = started;
                    if let Err(error) =
                        thread::Builder::new().spawn_scoped(scope, move || self.work(slot))
                    {
                        eprintln!(
                            "pd2-launcher: direct download: no thread for connection {}: {error}",
                            slot + 1
                        );
                        break;
                    }
                    started += 1;
                }
                self.wake.notify_all();
                started
            };
            if start(controller.connections()) == 0 {
                // Nothing could fetch: the round ends as a local failure.
                self.store_failed.store(true, Ordering::Relaxed);
            }
            let mut window: Option<Mark> = None;
            let mut settled = Instant::now() + self.limits.settle;
            loop {
                thread::sleep(TICK);
                (self.report)(Progress {
                    done: self.done.load(Ordering::Relaxed),
                    total: self.total,
                });
                let waiting = lock(&self.queue).len();
                if self.control.is_cancelled()
                    || self.store_failed.load(Ordering::Relaxed)
                    || (waiting == 0 && self.in_flight.load(Ordering::Relaxed) == 0)
                {
                    break;
                }
                let now = Instant::now();
                let connections = controller.connections();
                let Some(mark) = window else {
                    if now >= settled {
                        window = Some(self.mark(now));
                    }
                    continue;
                };
                let elapsed = now - mark.at;
                let pieces = self.fetched_pieces.load(Ordering::Relaxed) - mark.pieces;
                if elapsed < self.limits.window
                    || (pieces < WINDOW_PIECES_PER_CONNECTION * connections as u64
                        && elapsed < self.limits.longest_window)
                {
                    continue;
                }
                let end = self.mark(now);
                window = Some(end);
                let measured = Window {
                    connections,
                    bytes: end.fetched - mark.fetched,
                    elapsed,
                    requests: end.requests - mark.requests,
                    errors: end.errors - mark.errors,
                    starved: waiting < connections,
                };
                if let Some(change) = controller.observe(measured) {
                    eprintln!("pd2-launcher: direct download: {}", change.describe());
                    start(change.to);
                    window = None;
                    settled = now + self.limits.settle;
                }
            }
            self.finished.store(true, Ordering::Relaxed);
            self.wake.notify_all();
        });
    }

    fn work(&self, slot: usize) {
        let mut session = None;
        let mut random = Random::new();
        loop {
            {
                let mut parked = lock(&self.park);
                while slot >= self.target.load(Ordering::Relaxed)
                    && !self.finished.load(Ordering::Relaxed)
                {
                    parked = self
                        .wake
                        .wait_timeout(parked, TICK)
                        .unwrap_or_else(PoisonError::into_inner)
                        .0;
                }
            }
            if self.finished.load(Ordering::Relaxed)
                || self.control.is_cancelled()
                || self.store_failed.load(Ordering::Relaxed)
            {
                return;
            }
            let Some((product, piece)) = self.take() else {
                return;
            };
            // Given back also when the piece's work panics, so the round
            // never waits for it.
            let _in_flight = InFlight(&self.in_flight);
            self.piece(&mut session, &mut random, product, piece);
        }
    }

    /// The next piece, counted in flight; pieces of a product given up are
    /// dropped.
    fn take(&self) -> Option<(usize, usize)> {
        let mut queue = lock(&self.queue);
        loop {
            let (product, piece) = queue.pop_front()?;
            if self.products[product].given_up().is_some() {
                continue;
            }
            self.in_flight.fetch_add(1, Ordering::Relaxed);
            return Some((product, piece));
        }
    }

    fn piece(
        &self,
        session: &mut Option<Box<dyn Session>>,
        random: &mut Random,
        index: usize,
        piece: usize,
    ) {
        let product = &self.products[index];
        let download = product.download;
        let size = download.torrent.piece_size(piece) as usize;
        let cancelled = || self.control.is_cancelled();
        let mut last = None;
        for attempt in 0..=self.limits.retries {
            if cancelled() || product.given_up().is_some() {
                return;
            }
            if attempt > 0 && pause(self.limits.backoff * attempt as u32, self.control) {
                return;
            }
            if session.is_none() {
                *session = self.transport.session();
            }
            let Some(current) = session.as_mut() else {
                last = Some(RequestFailure::Transfer);
                continue;
            };
            // A retry asks with a query only to miss the CDN's cache, as
            // Blizzard's downloader does after a bad piece.
            let url = if attempt == 0 {
                format!("{}/{piece}", download.base)
            } else {
                format!("{}/{piece}?{}", download.base, random.salt())
            };
            let result = current.get(&PieceRequest {
                url: &url,
                cookie: download.cookie,
                limit: size,
                deadline: Instant::now() + self.limits.piece,
                cancelled: &cancelled,
            });
            if result == Err(RequestFailure::Cancelled) {
                return;
            }
            self.requests.fetch_add(1, Ordering::Relaxed);
            match result {
                Ok(data) if download.torrent.verify(piece, &data) => {
                    if download.write(piece, &data).is_err() {
                        eprintln!(
                            "pd2-launcher: direct download: a piece of {} could not be written",
                            download.torrent.name
                        );
                        self.store_failed.store(true, Ordering::Relaxed);
                        self.wake.notify_all();
                        return;
                    }
                    product.fetched_one();
                    self.fetched.fetch_add(size as u64, Ordering::Relaxed);
                    self.fetched_pieces.fetch_add(1, Ordering::Relaxed);
                    self.done.fetch_add(size as u64, Ordering::Relaxed);
                    return;
                }
                // Short, or not the piece: asked again.
                Ok(_) => last = Some(RequestFailure::Transfer),
                Err(failure) => {
                    // A connection that went quiet or broke is dropped with
                    // its pool, so the retry dials again.
                    if matches!(failure, RequestFailure::Timeout | RequestFailure::Transfer) {
                        *session = None;
                    }
                    last = Some(failure);
                }
            }
            self.errors.fetch_add(1, Ordering::Relaxed);
        }
        lock(&self.failed).push((index, piece));
        product.failed_one(last == Some(RequestFailure::Status(403)));
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Wait `duration`, or less when the user stops: whether they stopped.
fn pause(duration: Duration, control: &OperationControl) -> bool {
    let until = Instant::now() + duration;
    loop {
        if control.is_cancelled() {
            return true;
        }
        let now = Instant::now();
        if now >= until {
            return false;
        }
        thread::sleep((until - now).min(Duration::from_millis(50)));
    }
}

/// A small xorshift generator for the piece order and cache-busters; they
/// need to differ between runs, not to be secret.
struct Random(u64);

impl Random {
    fn new() -> Self {
        let seed = std::collections::hash_map::RandomState::new().hash_one(SystemTime::now());
        Self(seed | 1)
    }

    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, bound: usize) -> usize {
        (self.next() % bound as u64) as usize
    }

    fn shuffle<T>(&mut self, items: &mut [T]) {
        for index in (1..items.len()).rev() {
            items.swap(index, self.below(index + 1));
        }
    }

    fn salt(&mut self) -> String {
        const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz1234567890";
        (0..12)
            .map(|_| ALPHABET[self.below(ALPHABET.len())] as char)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        io::{BufRead, BufReader, Write},
        net::{TcpListener, TcpStream},
        sync::Arc,
    };

    use super::*;
    use crate::blizzard::torrent::tests::{FAKE_COOKIE, stub_bytes, torrent_bytes};

    const PIECE: usize = 1024;

    /// Short limits so the stalls and retries take milliseconds.
    const TEST_LIMITS: Limits = Limits {
        piece: Duration::from_millis(1500),
        idle: Duration::from_millis(300),
        retries: 3,
        backoff: Duration::from_millis(1),
        extra_rounds: 3,
        round_pause: Duration::from_millis(1),
        window: Duration::from_millis(100),
        longest_window: Duration::from_millis(400),
        settle: Duration::from_millis(50),
    };

    /// One request the fake host saw.
    #[derive(Clone, Debug)]
    struct Seen {
        path: String,
        query: Option<String>,
        headers: BTreeMap<String, String>,
    }

    enum Reply {
        Piece(Vec<u8>),
        Status(u16),
        /// Headers that promise more than is sent, then the connection
        /// closes.
        Short(Vec<u8>),
        /// Nothing at all for longer than the idle limit.
        Stall,
        Redirect(String),
    }

    type Answer = dyn Fn(&str, usize) -> Reply + Send + Sync;

    /// A local HTTP/1.1 host: `answer` gets each request's path and how
    /// many times that path was asked before.
    struct FakeHost {
        port: u16,
        seen: Arc<Mutex<Vec<Seen>>>,
    }

    impl FakeHost {
        fn start(answer: impl Fn(&str, usize) -> Reply + Send + Sync + 'static) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let seen = Arc::new(Mutex::new(Vec::new()));
            let answer: Arc<Answer> = Arc::new(answer);
            let recorded = Arc::clone(&seen);
            thread::spawn(move || {
                for stream in listener.incoming().flatten() {
                    let answer = Arc::clone(&answer);
                    let recorded = Arc::clone(&recorded);
                    thread::spawn(move || serve(stream, &*answer, &recorded));
                }
            });
            Self { port, seen }
        }

        fn base(&self, product: &str) -> String {
            format!("http://127.0.0.1:{}/{product}", self.port)
        }

        fn seen(&self) -> Vec<Seen> {
            lock(&self.seen).clone()
        }

        fn asked(&self, path: &str) -> usize {
            self.seen().iter().filter(|seen| seen.path == path).count()
        }
    }

    fn serve(stream: TcpStream, answer: &Answer, seen: &Mutex<Vec<Seen>>) {
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut writer = stream;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                return;
            }
            let target = line.split_whitespace().nth(1).unwrap_or("").to_owned();
            let mut headers = BTreeMap::new();
            loop {
                let mut header = String::new();
                if reader.read_line(&mut header).unwrap_or(0) == 0 {
                    return;
                }
                let header = header.trim_end();
                if header.is_empty() {
                    break;
                }
                if let Some((name, value)) = header.split_once(':') {
                    headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
                }
            }
            let (path, query) = match target.split_once('?') {
                Some((path, query)) => (path.to_owned(), Some(query.to_owned())),
                None => (target.clone(), None),
            };
            let before = {
                let mut seen = lock(seen);
                let before = seen.iter().filter(|earlier| earlier.path == path).count();
                seen.push(Seen {
                    path: path.clone(),
                    query,
                    headers,
                });
                before
            };
            let written = match answer(&path, before) {
                Reply::Piece(data) => writer
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\r\n",
                            data.len()
                        )
                        .as_bytes(),
                    )
                    .and_then(|()| writer.write_all(&data)),
                Reply::Status(status) => writer.write_all(
                    format!("HTTP/1.1 {status} No\r\nContent-Length: 0\r\n\r\n").as_bytes(),
                ),
                Reply::Redirect(location) => writer.write_all(
                    format!(
                        "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\n\r\n"
                    )
                    .as_bytes(),
                ),
                Reply::Short(data) => {
                    let _ = writer.write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
                            data.len() + 100
                        )
                        .as_bytes(),
                    );
                    let _ = writer.write_all(&data);
                    return;
                }
                Reply::Stall => {
                    thread::sleep(Duration::from_secs(3));
                    return;
                }
            };
            if written.is_err() {
                return;
            }
        }
    }

    /// One synthetic product: its torrent, payload and files on disk.
    struct Product {
        torrent: Torrent,
        payload: Vec<u8>,
        directory: tempfile::TempDir,
        layout: Vec<(&'static str, usize)>,
    }

    impl Product {
        /// A payload of `length` bytes in three files, the last not kept.
        fn new(seed: u8, length: usize, base: &str) -> Self {
            let payload: Vec<u8> = (0..length)
                .map(|index| (index as u8).wrapping_mul(31).wrapping_add(seed))
                .collect();
            let first = length / 3;
            let mac = 700.min(length - first - 1);
            let layout = vec![
                ("one.bin", first),
                ("Installer Tome.mpq", length - first - mac),
                ("Mac.app/skipped", mac),
            ];
            let stub = stub_bytes(
                &torrent_bytes("Synthetic", base, PIECE, &layout, &payload),
                FAKE_COOKIE,
            );
            Self {
                torrent: Torrent::from_stub(&stub).unwrap(),
                payload,
                directory: tempfile::tempdir().unwrap(),
                layout,
            }
        }

        /// The kept files, opened or made at full length.
        fn files(&self) -> Vec<Option<PayloadFile>> {
            self.layout
                .iter()
                .map(|(name, length)| {
                    (!name.starts_with("Mac.app/")).then(|| {
                        let path = self.directory.path().join(name);
                        let fresh = !path.exists();
                        let file = std::fs::OpenOptions::new()
                            .read(true)
                            .write(true)
                            .create(true)
                            .truncate(false)
                            .open(&path)
                            .unwrap();
                        file.set_len(*length as u64).unwrap();
                        PayloadFile { file, fresh }
                    })
                })
                .collect()
        }

        /// The bytes the kept files must hold.
        fn expected(&self) -> Vec<(String, Vec<u8>)> {
            let mut at = 0;
            let mut out = Vec::new();
            for (name, length) in &self.layout {
                if !name.starts_with("Mac.app/") {
                    out.push(((*name).to_owned(), self.payload[at..at + length].to_vec()));
                }
                at += length;
            }
            out
        }

        fn on_disk(&self) -> Vec<(String, Vec<u8>)> {
            self.expected()
                .into_iter()
                .map(|(name, _)| {
                    let bytes = std::fs::read(self.directory.path().join(&name)).unwrap();
                    (name, bytes)
                })
                .collect()
        }

        fn piece(&self, index: usize) -> Vec<u8> {
            self.payload[index * PIECE..((index + 1) * PIECE).min(self.payload.len())].to_vec()
        }
    }

    fn index(path: &str) -> usize {
        path.rsplit('/').next().unwrap().parse().unwrap()
    }

    fn transport() -> HttpTransport {
        HttpTransport {
            host: "127.0.0.1",
            port: None,
            path_prefix: "/",
            no_proxy: true,
            ..HttpTransport::new(TEST_LIMITS.idle)
        }
    }

    fn run(
        host: &FakeHost,
        products: &[(&Product, &str)],
        control: &OperationControl,
    ) -> Result<Summary, Cancelled> {
        let cookie = AccessCookie::for_tests(FAKE_COOKIE, 1_900_000_000);
        let files = products
            .iter()
            .map(|(product, _)| product.files())
            .collect::<Vec<_>>();
        let bases = products
            .iter()
            .map(|(_, name)| host.base(name))
            .collect::<Vec<_>>();
        let downloads = products
            .iter()
            .enumerate()
            .map(|(number, (product, _))| Download {
                torrent: &product.torrent,
                cookie: &cookie,
                base: &bases[number],
                files: &files[number],
            })
            .collect::<Vec<_>>();
        let last = Mutex::new(None);
        let result = fetch(
            &downloads,
            &transport(),
            &TEST_LIMITS,
            control,
            &|progress| {
                *lock(&last) = Some(progress);
            },
        );
        if let Some(progress) = *lock(&last) {
            assert!(progress.done <= progress.total);
        }
        result
    }

    #[test]
    fn two_products_download_at_the_same_time() {
        let host_products: Arc<Mutex<BTreeMap<String, Vec<Vec<u8>>>>> = Arc::default();
        let pieces = Arc::clone(&host_products);
        let host = FakeHost::start(move |path, _| {
            let product = path.split('/').nth(1).unwrap().to_owned();
            Reply::Piece(lock(&pieces)[&product][index(path)].clone())
        });
        let base_game = Product::new(1, 40 * PIECE + 123, &host.base("D2"));
        let expansion = Product::new(2, 25 * PIECE + 7, &host.base("LOD"));
        for (name, product) in [("D2", &base_game), ("LOD", &expansion)] {
            lock(&host_products).insert(
                name.to_owned(),
                (0..product.torrent.piece_count())
                    .map(|piece| product.piece(piece))
                    .collect(),
            );
        }
        let summary = run(
            &host,
            &[(&base_game, "D2"), (&expansion, "LOD")],
            &OperationControl::default(),
        )
        .unwrap();
        assert_eq!(summary.outcomes, [Outcome::Complete, Outcome::Complete]);
        assert_eq!(base_game.on_disk(), base_game.expected());
        assert_eq!(expansion.on_disk(), expansion.expected());
        assert_eq!(summary.resumed, 0);
        let seen = host.seen();
        // Every request carries the cookie and the downloader's headers.
        for request in &seen {
            assert_eq!(request.headers["cookie"], FAKE_COOKIE);
            assert_eq!(request.headers["pragma"], "no-cache");
            assert_eq!(request.headers["accept-encoding"], "identity");
            assert!(
                request.headers[zed_reqwest::header::USER_AGENT.as_str()]
                    .starts_with("pd2-launcher/")
            );
            assert!(request.query.is_none());
        }
        // A piece only in the file that is not kept is never asked for.
        let last_d2 = base_game.torrent.piece_count() - 1;
        assert_eq!(host.asked(&format!("/D2/{last_d2}")), 0);
        // Both products were asked for from the start, interleaved.
        let first = &seen[..16.min(seen.len())];
        assert!(first.iter().any(|request| request.path.starts_with("/D2/")));
        assert!(
            first
                .iter()
                .any(|request| request.path.starts_with("/LOD/"))
        );
    }

    #[test]
    fn a_wall_of_403_gives_one_product_up_and_the_other_finishes() {
        let base_game = Product::new(3, 30 * PIECE, "http://placeholder");
        let pieces = (0..base_game.torrent.piece_count())
            .map(|piece| base_game.piece(piece))
            .collect::<Vec<_>>();
        let host = FakeHost::start(move |path, _| {
            if path.starts_with("/LOD/") {
                Reply::Status(403)
            } else {
                Reply::Piece(pieces[index(path)].clone())
            }
        });
        let expansion = Product::new(4, 30 * PIECE, "http://placeholder");
        let summary = run(
            &host,
            &[(&base_game, "D2"), (&expansion, "LOD")],
            &OperationControl::default(),
        )
        .unwrap();
        assert_eq!(summary.outcomes, [Outcome::Complete, Outcome::Refused]);
        assert_eq!(base_game.on_disk(), base_game.expected());
        // Given up after 8 failed pieces (4 tries each), not every piece.
        let refused = host
            .seen()
            .iter()
            .filter(|request| request.path.starts_with("/LOD/"))
            .count();
        assert!(refused < 30 * 4, "{refused}");
    }

    #[test]
    fn a_silent_stall_a_short_body_and_a_bad_piece_are_asked_again() {
        let product = Product::new(5, 12 * PIECE, "http://placeholder");
        let pieces = (0..product.torrent.piece_count())
            .map(|piece| product.piece(piece))
            .collect::<Vec<_>>();
        let host = FakeHost::start(move |path, before| match (index(path), before) {
            (1, 0) => Reply::Stall,
            (2, 0) => Reply::Short(pieces[2][..100].to_vec()),
            (3, 0) => {
                let mut bad = pieces[3].clone();
                bad[0] ^= 1;
                Reply::Piece(bad)
            }
            (4, 0) => Reply::Status(503),
            (piece, _) => Reply::Piece(pieces[piece].clone()),
        });
        let summary = run(&host, &[(&product, "D2")], &OperationControl::default()).unwrap();
        assert_eq!(summary.outcomes, [Outcome::Complete]);
        assert_eq!(product.on_disk(), product.expected());
        for piece in 1..=4 {
            let asked = host
                .seen()
                .into_iter()
                .filter(|request| request.path == format!("/D2/{piece}"))
                .collect::<Vec<_>>();
            assert_eq!(asked.len(), 2, "piece {piece}");
            // The retry carries a cache-buster.
            assert!(asked[0].query.is_none());
            assert_eq!(asked[1].query.as_ref().map(String::len), Some(12));
        }
    }

    #[test]
    fn pieces_that_never_arrive_get_further_rounds() {
        let product = Product::new(6, 10 * PIECE, "http://placeholder");
        let pieces = (0..product.torrent.piece_count())
            .map(|piece| product.piece(piece))
            .collect::<Vec<_>>();
        // Piece 3 fails all four tries of the first round.
        let host = FakeHost::start(move |path, before| match (index(path), before) {
            (3, 0..=3) => Reply::Status(500),
            (piece, _) => Reply::Piece(pieces[piece].clone()),
        });
        let summary = run(&host, &[(&product, "D2")], &OperationControl::default()).unwrap();
        assert_eq!(summary.outcomes, [Outcome::Complete]);
        assert_eq!(host.asked("/D2/3"), 5);
        assert_eq!(product.on_disk(), product.expected());

        // A piece that never arrives ends the rounds once one gets no
        // further: the product is incomplete, the others kept.
        let stuck = Product::new(7, 10 * PIECE, "http://placeholder");
        let pieces = (0..stuck.torrent.piece_count())
            .map(|piece| stuck.piece(piece))
            .collect::<Vec<_>>();
        let host = FakeHost::start(move |path, _| match index(path) {
            5 => Reply::Status(500),
            piece => Reply::Piece(pieces[piece].clone()),
        });
        let summary = run(&host, &[(&stuck, "D2")], &OperationControl::default()).unwrap();
        assert_eq!(summary.outcomes, [Outcome::Incomplete]);
        // The first round and one more, which got no further.
        assert_eq!(host.asked("/D2/5"), 8);
    }

    #[test]
    fn a_cookie_that_expires_during_the_download_is_told_apart() {
        let product = Product::new(8, 40 * PIECE, "http://placeholder");
        let pieces = (0..product.torrent.piece_count())
            .map(|piece| product.piece(piece))
            .collect::<Vec<_>>();
        let served = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&served);
        let host = FakeHost::start(move |path, _| {
            if counter.fetch_add(1, Ordering::Relaxed) < 10 {
                Reply::Piece(pieces[index(path)].clone())
            } else {
                Reply::Status(403)
            }
        });
        let summary = run(&host, &[(&product, "D2")], &OperationControl::default()).unwrap();
        assert_eq!(summary.outcomes, [Outcome::Expired]);
        assert!(summary.fetched > 0);
    }

    #[test]
    fn a_stop_keeps_the_files_and_the_next_run_resumes() {
        let product = Product::new(9, 30 * PIECE, "http://placeholder");
        let pieces = (0..product.torrent.piece_count())
            .map(|piece| product.piece(piece))
            .collect::<Vec<_>>();
        let control = OperationControl::default();
        let stopper = control.clone();
        let served = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&served);
        let host = FakeHost::start(move |path, _| {
            // The user stops after a few pieces arrived.
            if counter.fetch_add(1, Ordering::Relaxed) == 6 {
                stopper.request_cancel();
            }
            Reply::Piece(pieces[index(path)].clone())
        });
        assert_eq!(
            run(&host, &[(&product, "D2")], &control).unwrap_err(),
            Cancelled
        );
        let first = host.seen().len();
        assert!(first < 30, "{first}");

        // The files are kept; the next run checks them and asks only for
        // what is missing.
        let summary = run(&host, &[(&product, "D2")], &OperationControl::default()).unwrap();
        assert_eq!(summary.outcomes, [Outcome::Complete]);
        assert!(summary.resumed > 0);
        assert_eq!(product.on_disk(), product.expected());
        let second = host.seen().len() - first;
        assert!(second < product.torrent.piece_count(), "{second}");
        // A finished download checks everything and fetches only the piece
        // that also holds bytes of the file not kept, which the disk cannot
        // check.
        let again = run(&host, &[(&product, "D2")], &OperationControl::default()).unwrap();
        assert_eq!(again.outcomes, [Outcome::Complete]);
        assert_eq!(again.fetched, PIECE as u64);
    }

    #[test]
    fn a_redirect_must_keep_the_piece_hosts_port_and_path() {
        let allowed = |url: &str| {
            redirect_allowed(
                &zed_reqwest::Url::parse(url).unwrap(),
                crate::blizzard::PIECE_HOST,
                Some(80),
                crate::blizzard::PIECE_PATH_PREFIX,
            )
        };
        let base = "rogue.blizzard.com.edgesuite.net";
        assert!(allowed(&format!(
            "http://{base}/applications/Diablo2/1.14B/D2/enUS/7"
        )));
        assert!(allowed(&format!(
            "http://{base}:80/applications/Diablo2/1.14B/LOD/enUS/7"
        )));
        assert!(!allowed(&format!(
            "http://{base}:8080/applications/Diablo2/1.14B/D2/enUS/7"
        )));
        assert!(!allowed(&format!("http://{base}/other/7")));
        assert!(!allowed(&format!(
            "https://{base}/applications/Diablo2/1.14B/D2/enUS/7"
        )));
        assert!(!allowed(
            "http://example.com/applications/Diablo2/1.14B/D2/enUS/7"
        ));
    }

    #[test]
    fn redirects_stay_on_the_piece_host() {
        let product = Product::new(10, 4 * PIECE, "http://placeholder");
        let pieces = (0..product.torrent.piece_count())
            .map(|piece| product.piece(piece))
            .collect::<Vec<_>>();
        let port = Arc::new(AtomicUsize::new(0));
        let own = Arc::clone(&port);
        let host = FakeHost::start(move |path, _| {
            let port = own.load(Ordering::Relaxed);
            match path {
                // Moved on the same host: followed.
                "/D2/0" => Reply::Redirect(format!("http://127.0.0.1:{port}/moved/0")),
                // Another host name: refused, so piece 1 never arrives.
                "/D2/1" => Reply::Redirect(format!("http://localhost:{port}/moved/1")),
                "/moved/0" => Reply::Piece(pieces[0].clone()),
                path => Reply::Piece(pieces[index(path)].clone()),
            }
        });
        port.store(usize::from(host.port), Ordering::Relaxed);
        let summary = run(&host, &[(&product, "D2")], &OperationControl::default()).unwrap();
        assert_eq!(summary.outcomes, [Outcome::Incomplete]);
        assert_eq!(host.asked("/moved/0"), 1);
        assert_eq!(host.asked("/moved/1"), 0);
        assert!(host.asked("/D2/1") >= 4);
    }
}
