//! How many connections the direct download keeps open at once.
//!
//! The piece host limits each connection (to about 2 to 2.7 MB/s), not the
//! client: the total speed grows with the number of connections until the
//! line or the host is saturated. Diablo II and Lord of Destruction share one
//! pool, so one count governs both.
//!
//! The count climbs while each step pays: it doubles while a doubling
//! raises the total by half or more, then adds a quarter while a step still
//! raises it clearly. A step without a clear gain is saturation: the count
//! goes back to the step before and holds there. Failed requests cut it in
//! half at once, and so does a held speed that falls, after which it climbs
//! again at once. A held count is probed from time to time, as the network
//! can change (a phone that changes Wi-Fi): with a doubling, whose gain or
//! none stands out from the noise of a short window, or with a quarter more
//! once failed requests showed a limit of the host.

use std::time::Duration;

/// Connections a download starts with.
pub(crate) const START_CONNECTIONS: usize = 4;
/// The most connections at once: 64 connections reach about 80 MB/s, and
/// the piece host sets no total limit of its own below that.
pub(crate) const MAX_CONNECTIONS: usize = 64;
pub(crate) const MIN_CONNECTIONS: usize = 1;

/// A doubling that raises the speed by at least this share doubles again.
const DOUBLING_GAIN: f64 = 0.5;
/// A step that raises the speed by less than this share is no clear gain.
const CLEAR_GAIN: f64 = 0.10;
/// Failed requests, as a share of a window's requests (and at least
/// [`ERROR_MIN`]), that cut the count.
const ERROR_SHARE: f64 = 0.10;
const ERROR_MIN: u64 = 2;
/// A held speed under this share of its reference, [`FALLS_TO_CUT`] windows
/// in a row, cuts the count.
const FALL_SHARE: f64 = 0.7;
const FALLS_TO_CUT: u32 = 2;
/// How long a count is held before one step more is tried.
const PROBE_AFTER: Duration = Duration::from_secs(20);

/// What one measuring window saw.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Window {
    /// The count during the window.
    pub(crate) connections: usize,
    /// Bytes of pieces fetched and verified (not ones found on disk).
    pub(crate) bytes: u64,
    pub(crate) elapsed: Duration,
    /// Requests that ended in the window, and how many of them failed: a
    /// timeout, a broken or short body, a piece that failed its check, or
    /// an answer other than 200 (403, 429, 5xx).
    pub(crate) requests: u64,
    pub(crate) errors: u64,
    /// Fewer pieces waited than there were connections: the speed says
    /// nothing about the count.
    pub(crate) starved: bool,
}

impl Window {
    /// Bytes per second.
    fn speed(&self) -> f64 {
        let seconds = self.elapsed.as_secs_f64();
        if seconds > 0.0 {
            self.bytes as f64 / seconds
        } else {
            0.0
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Reason {
    /// The last step raised the speed.
    Climb,
    /// The last step gave no clear gain: back to the count before it.
    Saturated,
    /// Too many requests failed.
    Errors,
    /// The held speed fell.
    SpeedFell,
    /// One step more after holding.
    Probe,
}

/// One change of the count and the speeds that caused it, in bytes per
/// second.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Change {
    pub(crate) from: usize,
    pub(crate) to: usize,
    pub(crate) reason: Reason,
    pub(crate) speed: f64,
    /// The speed it was compared with, when there was one.
    pub(crate) compared: Option<f64>,
}

impl Change {
    /// One line for the log.
    pub(crate) fn describe(&self) -> String {
        let megabytes = |speed: f64| speed / 1_000_000.0;
        let compared = self
            .compared
            .map(|compared| format!(", against {:.1} MB/s", megabytes(compared)))
            .unwrap_or_default();
        format!(
            "{} -> {} connections ({:?}: {:.1} MB/s{compared})",
            self.from,
            self.to,
            self.reason,
            megabytes(self.speed)
        )
    }
}

#[derive(Clone, Copy, Debug)]
struct Level {
    connections: usize,
    speed: f64,
}

#[derive(Clone, Copy, Debug)]
enum Mode {
    /// Climbing: doubling, or adding a quarter. `base` is the level before
    /// the step being measured (none in the first window).
    Climbing { doubling: bool, base: Option<Level> },
    /// Holding the count: `reference` is the speed held (none until the
    /// first window), `held` how long it has been held.
    Holding {
        reference: Option<f64>,
        falls: u32,
        held: Duration,
    },
}

/// The count, decided one window at a time.
#[derive(Debug)]
pub(crate) struct Controller {
    connections: usize,
    mode: Mode,
    /// Failed requests cut the count once: later climbs add a quarter at a
    /// time, never a doubling, so as not to run into that limit again.
    cautious: bool,
}

impl Default for Controller {
    fn default() -> Self {
        Self::new()
    }
}

impl Controller {
    pub(crate) fn new() -> Self {
        Self {
            connections: START_CONNECTIONS,
            mode: Mode::Climbing {
                doubling: true,
                base: None,
            },
            cautious: false,
        }
    }

    pub(crate) fn connections(&self) -> usize {
        self.connections
    }

    /// Take one window's measurement; the change it causes, if any. A
    /// window measured at another count than the current one is ignored.
    pub(crate) fn observe(&mut self, window: Window) -> Option<Change> {
        if window.connections != self.connections {
            return None;
        }
        let speed = window.speed();
        if window.errors >= ERROR_MIN
            && window.errors as f64 >= ERROR_SHARE * window.requests as f64
        {
            self.mode = holding(None);
            self.cautious = true;
            return self.change(half(self.connections), Reason::Errors, speed, None);
        }
        if window.starved {
            return None;
        }
        match self.mode {
            Mode::Climbing { doubling, base } => {
                let doubling = match base {
                    None => doubling,
                    Some(base) => {
                        let gain = gain(speed, base.speed);
                        if gain < CLEAR_GAIN {
                            self.mode = holding(Some(base.speed));
                            return self.change(
                                base.connections,
                                Reason::Saturated,
                                speed,
                                Some(base.speed),
                            );
                        }
                        doubling && gain >= DOUBLING_GAIN
                    }
                };
                let to = if doubling {
                    self.connections * 2
                } else {
                    self.connections + step(self.connections)
                }
                .min(MAX_CONNECTIONS);
                if to == self.connections {
                    self.mode = holding(Some(speed));
                    return None;
                }
                self.mode = Mode::Climbing {
                    doubling,
                    base: Some(Level {
                        connections: self.connections,
                        speed,
                    }),
                };
                self.change(to, Reason::Climb, speed, base.map(|base| base.speed))
            }
            Mode::Holding {
                reference,
                falls,
                held,
            } => {
                let held = held + window.elapsed;
                let Some(reference) = reference else {
                    self.mode = Mode::Holding {
                        reference: Some(speed),
                        falls: 0,
                        held,
                    };
                    return None;
                };
                if speed < FALL_SHARE * reference {
                    let falls = falls + 1;
                    if falls >= FALLS_TO_CUT && self.connections > MIN_CONNECTIONS {
                        // Measured again from the lower count, then climbed
                        // at once while steps pay: the network may only
                        // have had a moment's trouble.
                        self.mode = Mode::Climbing {
                            doubling: !self.cautious,
                            base: None,
                        };
                        return self.change(
                            half(self.connections),
                            Reason::SpeedFell,
                            speed,
                            Some(reference),
                        );
                    }
                    self.mode = Mode::Holding {
                        reference: Some(reference),
                        falls,
                        held,
                    };
                    return None;
                }
                let reference = reference * 0.8 + speed * 0.2;
                if held >= PROBE_AFTER && self.connections < MAX_CONNECTIONS {
                    let doubling = !self.cautious;
                    let to = if doubling {
                        self.connections * 2
                    } else {
                        self.connections + step(self.connections)
                    }
                    .min(MAX_CONNECTIONS);
                    self.mode = Mode::Climbing {
                        doubling,
                        base: Some(Level {
                            connections: self.connections,
                            speed: reference,
                        }),
                    };
                    return self.change(to, Reason::Probe, speed, Some(reference));
                }
                self.mode = Mode::Holding {
                    reference: Some(reference),
                    falls: 0,
                    held,
                };
                None
            }
        }
    }

    fn change(
        &mut self,
        to: usize,
        reason: Reason,
        speed: f64,
        compared: Option<f64>,
    ) -> Option<Change> {
        let from = self.connections;
        self.connections = to.clamp(MIN_CONNECTIONS, MAX_CONNECTIONS);
        (self.connections != from).then_some(Change {
            from,
            to: self.connections,
            reason,
            speed,
            compared,
        })
    }
}

const fn holding(reference: Option<f64>) -> Mode {
    Mode::Holding {
        reference,
        falls: 0,
        held: Duration::ZERO,
    }
}

/// A quarter of the count, at least one.
fn step(connections: usize) -> usize {
    (connections / 4).max(1)
}

fn half(connections: usize) -> usize {
    (connections / 2).max(MIN_CONNECTIONS)
}

/// How much faster `speed` is than `base`, as a share.
fn gain(speed: f64, base: f64) -> f64 {
    if base > 0.0 {
        speed / base - 1.0
    } else if speed > 0.0 {
        f64::INFINITY
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MB: f64 = 1_000_000.0;
    const PIECE: f64 = 262_144.0;
    const WINDOW: Duration = Duration::from_millis(1500);

    /// A simulated piece host and line: a speed per connection, a total
    /// limit, and above `error_from` connections a share of the requests
    /// refused (none of their bytes arrive).
    #[derive(Clone, Copy)]
    struct Capacity {
        per_connection: f64,
        line: f64,
        error_from: Option<usize>,
    }

    impl Capacity {
        fn window(self, connections: usize) -> Window {
            let offered = (connections as f64 * self.per_connection).min(self.line);
            let refused = self.error_from.map_or(0.0, |limit| {
                connections.saturating_sub(limit) as f64 / connections as f64
            });
            let seconds = WINDOW.as_secs_f64();
            let requests = (offered * seconds / PIECE).max(1.0);
            Window {
                connections,
                bytes: (offered * (1.0 - refused) * seconds) as u64,
                elapsed: WINDOW,
                requests: requests as u64,
                errors: (requests * refused).round() as u64,
                starved: false,
            }
        }
    }

    /// The count before each window, for `windows` windows of `capacity`
    /// (which may change with the window's number).
    fn run(
        controller: &mut Controller,
        windows: usize,
        capacity: impl Fn(usize) -> Capacity,
    ) -> Vec<usize> {
        let mut counts = Vec::new();
        for number in 0..windows {
            counts.push(controller.connections());
            let window = capacity(number).window(controller.connections());
            if let Some(change) = controller.observe(window) {
                assert!(!change.describe().is_empty());
            }
        }
        counts
    }

    #[test]
    fn it_doubles_to_saturation_then_holds() {
        // A phone on Wi-Fi: 2 MB/s per connection, the line at 40 MB/s.
        let capacity = Capacity {
            per_connection: 2.0 * MB,
            line: 40.0 * MB,
            error_from: None,
        };
        let mut controller = Controller::new();
        let counts = run(&mut controller, 60, |_| capacity);
        // Doubling from 4, then a quarter more past 32 gave nothing.
        assert_eq!(counts[..6], [4, 8, 16, 32, 40, 32]);
        // Held at a count that saturates the line; each probe (a doubling)
        // goes back.
        assert!(counts[6..].iter().all(|count| [32, 64].contains(count)));
        assert_eq!(controller.connections(), 32);
        let saturated = counts[6..].iter().filter(|count| **count == 32).count();
        assert!(saturated * 10 >= (counts.len() - 6) * 8, "{counts:?}");
    }

    #[test]
    fn a_fast_line_reaches_the_maximum_quickly() {
        // About 1.3 to 2 MB/s per connection on a 94 MB/s line.
        let capacity = Capacity {
            per_connection: 1.5 * MB,
            line: 94.0 * MB,
            error_from: None,
        };
        let mut controller = Controller::new();
        let counts = run(&mut controller, 12, |_| capacity);
        assert_eq!(counts[..5], [4, 8, 16, 32, 64]);
        assert!(counts[5..].iter().all(|count| *count == MAX_CONNECTIONS));
    }

    #[test]
    fn a_slow_line_stops_early() {
        let capacity = Capacity {
            per_connection: 2.0 * MB,
            line: 6.0 * MB,
            error_from: None,
        };
        let mut controller = Controller::new();
        let counts = run(&mut controller, 10, |_| capacity);
        // 4 connections already fill the line: 8 gave nothing.
        assert_eq!(counts[..3], [4, 8, 4]);
        assert_eq!(controller.connections(), 4);
    }

    #[test]
    fn refused_requests_cut_the_count_and_it_stays_near_the_limit() {
        // A host that refuses part of the requests above 24 connections.
        let capacity = Capacity {
            per_connection: 2.0 * MB,
            line: 200.0 * MB,
            error_from: Some(24),
        };
        let mut controller = Controller::new();
        let counts = run(&mut controller, 400, |_| capacity);
        assert_eq!(counts[..5], [4, 8, 16, 32, 16]);
        // Every window with too many refusals is followed by a halved count.
        for pair in counts.windows(2) {
            let window = capacity.window(pair[0]);
            if window.errors >= ERROR_MIN
                && window.errors as f64 >= ERROR_SHARE * window.requests as f64
            {
                assert_eq!(pair[1], pair[0] / 2, "{counts:?}");
            }
        }
        let above = counts.iter().filter(|count| **count > 24).count();
        assert!(above * 100 <= counts.len() * 15, "{counts:?}");
        assert!(counts[5..].iter().all(|count| *count >= 12), "{counts:?}");
    }

    #[test]
    fn a_falling_speed_cuts_the_count_and_it_climbs_again() {
        let fast = Capacity {
            per_connection: 2.0 * MB,
            line: 40.0 * MB,
            error_from: None,
        };
        // The phone moves to a slower network after window 20, and back
        // after window 60.
        let slow = Capacity {
            line: 8.0 * MB,
            ..fast
        };
        let mut controller = Controller::new();
        let counts = run(&mut controller, 120, |number| {
            if (20..60).contains(&number) {
                slow
            } else {
                fast
            }
        });
        // Held at 32; window 19 is a probe that went back.
        assert_eq!(counts[18..20], [32, 64]);
        // Two slow windows, then half: measured again, and a doubling gave
        // nothing.
        assert_eq!(counts[20..26], [32, 32, 16, 32, 16, 16]);
        assert!(counts[26..60].iter().all(|count| [16, 32].contains(count)));
        // Faster again: a probe finds the gain, and it climbs until the line
        // (20 connections of 2 MB/s) is full again.
        assert!(counts[100..].iter().all(|count| *count >= 20), "{counts:?}");
    }

    #[test]
    fn a_short_dip_is_climbed_back_at_once() {
        // 16 connections fill the line at 86 MB/s, then the speed falls to
        // 14 MB/s for a moment.
        let line = Capacity {
            per_connection: 5.4 * MB,
            line: 86.0 * MB,
            error_from: None,
        };
        let dip = Capacity {
            line: 14.0 * MB,
            ..line
        };
        let mut controller = Controller::new();
        let counts = run(&mut controller, 20, |number| {
            if (10..12).contains(&number) {
                dip
            } else {
                line
            }
        });
        assert_eq!(counts[..5], [4, 8, 16, 32, 16]);
        assert_eq!(counts[10..16], [16, 16, 8, 16, 32, 16]);
        assert!(counts[15..].iter().all(|count| *count == 16));
    }

    #[test]
    fn starved_windows_and_old_counts_change_nothing() {
        let mut controller = Controller::new();
        let window = Window {
            connections: START_CONNECTIONS,
            bytes: 1,
            elapsed: WINDOW,
            requests: 1,
            errors: 0,
            starved: true,
        };
        assert!(controller.observe(window).is_none());
        let other = Window {
            connections: 9,
            starved: false,
            ..window
        };
        assert!(controller.observe(other).is_none());
        assert_eq!(controller.connections(), START_CONNECTIONS);
        // Errors count even when starved; never below one connection.
        let failing = Window {
            connections: START_CONNECTIONS,
            errors: 5,
            requests: 5,
            ..window
        };
        let mut floor = Controller::new();
        assert_eq!(floor.observe(failing).unwrap().to, 2);
        assert_eq!(
            floor
                .observe(Window {
                    connections: 2,
                    ..failing
                })
                .unwrap()
                .to,
            1
        );
        assert!(
            floor
                .observe(Window {
                    connections: 1,
                    ..failing
                })
                .is_none()
        );
        assert_eq!(floor.connections(), MIN_CONNECTIONS);
    }
}
