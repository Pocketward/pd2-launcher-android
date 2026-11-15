//! Box64's dynamic recompiler settings for the Windows programs that the
//! Launcher runs on the Android host.
//!
//! Box64 (`wowbox64.dll`) reads its `BOX64_DYNAREC_*` settings from the
//! environment of each Wine process, and a program that another one starts
//! inherits that environment. So all the programs of one Wine run share one
//! value for each setting, and `BOX64_DYNAREC_PROGRAMS` (our Box64 patches
//! 0006 and 0009) says which program gets which of them: an entry `name`
//! gets every dynarec setting of the environment, an entry
//! `name:SETTING:SETTING` only those it names, and a program that the list
//! does not name gets Box64's defaults.
//!
//! This module holds which program needs which setting, and why
//! ([`Dynarec`], [`Run`]), and makes the environment of a run from that
//! ([`settings`]).

use std::collections::BTreeSet;

use crate::wine_environment::{Kind, Setting};

/// The start of each variable's name here.
const PREFIX: &str = "BOX64_DYNAREC_";

/// A dynamic recompiler setting that some program gets with another value
/// than Box64's default. Each has one value, since the programs of a run
/// share the variable; a program that does not get it has Box64's default.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum Dynarec {
    /// A call and its return go directly to the translated code, not through
    /// the jump table. For the game.
    CallRet,
    /// PD2's known x86 functions (D2Lang's string lookup, Storm's explode)
    /// run as native code (our Box64 patches 0012 to 0014). Off in Box64 by
    /// default; only the game gets it, so no other program checks for them.
    NativeX86,
    /// Exact x86 flags at every instruction. Inno Setup's program and the
    /// `.tmp` copy it runs stop with an access violation without it.
    SafeFlags,
    /// The x86 memory order. The official launcher's multithreaded .NET
    /// (JIT-compiled code, the garbage collector, the thread pool) relies
    /// on it.
    StrongMem,
    /// Code that a program writes runs at once. The official launcher's
    /// .NET needs that too.
    Wait,
}

impl Dynarec {
    /// Box64's name for it, after [`PREFIX`].
    const fn name(self) -> &'static str {
        match self {
            Self::CallRet => "CALLRET",
            Self::NativeX86 => "NATIVEX86",
            Self::SafeFlags => "SAFEFLAGS",
            Self::StrongMem => "STRONGMEM",
            Self::Wait => "WAIT",
        }
    }

    /// The value that a program which gets it runs with.
    const fn value(self) -> &'static str {
        match self {
            Self::CallRet => "1",
            Self::NativeX86 => "1",
            Self::SafeFlags => "2",
            Self::StrongMem => "1",
            Self::Wait => "0",
        }
    }
}

/// A Windows program, by its file name, and the settings it runs with.
#[derive(Clone, Copy)]
struct Program {
    name: &'static str,
    settings: &'static [Dynarec],
}

/// PD2's installer as the Launcher runs it on Android
/// (`ANDROID_CLIENT_INSTALLER_NAME` in `install::adapters`), and the copy of
/// itself that Inno Setup runs.
const INSTALLER: [Program; 2] = [
    Program {
        name: "ProjectD2Installer.exe",
        settings: &[Dynarec::SafeFlags],
    },
    Program {
        name: "ProjectD2Installer.tmp",
        settings: &[Dynarec::SafeFlags],
    },
];

/// The official PD2 launcher and its updater. A launcher program of another
/// name (the player's Launcher program setting) is not one of them and runs
/// with Box64's defaults.
const OFFICIAL_LAUNCHER: [Program; 2] = [
    Program {
        name: "PD2Launcher.exe",
        settings: &[Dynarec::StrongMem, Dynarec::Wait],
    },
    Program {
        name: "UpdateUtility.exe",
        settings: &[Dynarec::StrongMem, Dynarec::Wait],
    },
];

/// The game. The game server limits the time from the request that creates
/// a game to the game's logon, and a create that takes longer fails with
/// "Failed to join game" (issue #17). Direct calls make the game's loading
/// in that time shorter. On a phone with an SM6115 chip the loading of a
/// single-player game entry went from 1.274 s to 1.122 s (10 entries each,
/// 2026-11-11).
///
/// Box64's `BIGBLOCK=3` is not here, because it changed nothing in the same
/// measurement, alone (1.261 s) or added to the direct calls (1.119 s).
///
/// `strict_memory_order` adds the x86 memory order (the Android app's Strict
/// CPU emulation, which no app asks for at present). It makes that loading
/// about 0.8 s longer, so on that chip each create then fails.
const fn game(strict_memory_order: bool) -> Program {
    Program {
        name: "Game.exe",
        settings: if strict_memory_order {
            &[Dynarec::CallRet, Dynarec::NativeX86, Dynarec::StrongMem]
        } else {
            &[Dynarec::CallRet, Dynarec::NativeX86]
        },
    }
}

/// A Wine run of the Launcher whose programs need Box64 settings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Run {
    /// PD2's installer.
    Installer,
    /// The official PD2 launcher, its updater, and a game that it starts,
    /// which gets what [`Run::Game`] gives it: it inherits the launcher's
    /// environment, so its entry names its own settings.
    OfficialLauncher { game_strict_memory_order: bool },
    /// The game, from the Launcher's Play.
    Game { strict_memory_order: bool },
}

impl Run {
    fn programs(self) -> Vec<Program> {
        match self {
            Self::Installer => INSTALLER.to_vec(),
            Self::OfficialLauncher {
                game_strict_memory_order,
            } => OFFICIAL_LAUNCHER
                .into_iter()
                .chain([game(game_strict_memory_order)])
                .collect(),
            Self::Game {
                strict_memory_order,
            } => vec![game(strict_memory_order)],
        }
    }
}

/// What the Wine of `run` gets on the Android host (`android`): the list of
/// its programs and each setting that one of them needs. Nothing on the
/// desktop, which has no Box64.
///
/// A program that gets every setting of the run is in the list by its name
/// alone, so it also keeps a dynarec setting that reaches its environment
/// from elsewhere (the app's own `BOX64_*` values, which pass through to
/// Wine, and the prefix's `HKCU\Environment`). An entry names its settings
/// only where the run has a setting that its program must not get.
///
/// Every list ends with `;`. A wowbox64 without our Box64 patch 0010
/// (upstream 8851be92) reads the value with no closing NUL into a buffer
/// that held a longer value before, so leftover bytes would join the last
/// name; the `;` puts them into a name of their own, which matches no
/// program.
pub(crate) fn settings(run: Run, android: bool) -> Vec<Setting> {
    if !android {
        return Vec::new();
    }
    let programs = run.programs();
    let all = programs
        .iter()
        .flat_map(|program| program.settings)
        .copied()
        .collect::<BTreeSet<_>>();
    let list = programs
        .iter()
        .map(|program| {
            let own = program.settings.iter().copied().collect::<BTreeSet<_>>();
            if own == all {
                format!("{};", program.name)
            } else {
                let names = own.iter().map(|setting| setting.name());
                format!("{}:{};", program.name, names.collect::<Vec<_>>().join(":"))
            }
        })
        .collect::<String>();
    std::iter::once(Setting::new(
        format!("{PREFIX}PROGRAMS"),
        list,
        Kind::Preference,
    ))
    .chain(all.into_iter().map(|setting| {
        Setting::new(
            format!("{PREFIX}{}", setting.name()),
            setting.value(),
            Kind::Preference,
        )
    }))
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The environment that Box64 reads for `run` on Android, by name.
    fn environment(run: Run) -> Vec<(String, String)> {
        let mut environment = settings(run, true)
            .into_iter()
            .map(|setting| {
                assert_eq!(setting.kind, Kind::Preference);
                (
                    setting.name.into_string().unwrap(),
                    setting.value.into_string().unwrap(),
                )
            })
            .collect::<Vec<_>>();
        environment.sort();
        environment
    }

    fn pairs(expected: &[(&str, &str)]) -> Vec<(String, String)> {
        expected
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect()
    }

    /// The settings that the entry of `program` in the list of `run` names;
    /// `None` for an entry that is the name alone.
    fn named(run: Run, program: &str) -> Option<BTreeSet<String>> {
        let environment = environment(run);
        let (_, list) = environment
            .iter()
            .find(|(name, _)| name == "BOX64_DYNAREC_PROGRAMS")
            .expect("the run has a program list");
        let entry = list
            .split(';')
            .find(|entry| entry.split(':').next() == Some(program))
            .unwrap_or_else(|| panic!("{program} is in {list}"));
        entry
            .split_once(':')
            .map(|(_, settings)| settings.split(':').map(str::to_owned).collect())
    }

    #[test]
    fn the_installer_and_its_inno_setup_copy_get_exact_flags() {
        assert_eq!(
            environment(Run::Installer),
            pairs(&[
                (
                    "BOX64_DYNAREC_PROGRAMS",
                    "ProjectD2Installer.exe;ProjectD2Installer.tmp;"
                ),
                ("BOX64_DYNAREC_SAFEFLAGS", "2"),
            ])
        );
    }

    #[test]
    fn a_play_gives_the_game_direct_calls_and_strict_memory_order_when_asked() {
        assert_eq!(
            environment(Run::Game {
                strict_memory_order: false
            }),
            pairs(&[
                ("BOX64_DYNAREC_CALLRET", "1"),
                ("BOX64_DYNAREC_NATIVEX86", "1"),
                ("BOX64_DYNAREC_PROGRAMS", "Game.exe;"),
            ])
        );
        assert_eq!(
            environment(Run::Game {
                strict_memory_order: true
            }),
            pairs(&[
                ("BOX64_DYNAREC_CALLRET", "1"),
                ("BOX64_DYNAREC_NATIVEX86", "1"),
                ("BOX64_DYNAREC_PROGRAMS", "Game.exe;"),
                ("BOX64_DYNAREC_STRONGMEM", "1"),
            ])
        );
    }

    #[test]
    fn the_official_launcher_and_a_game_that_it_starts_each_get_only_their_own_settings() {
        assert_eq!(
            environment(Run::OfficialLauncher {
                game_strict_memory_order: false
            }),
            pairs(&[
                ("BOX64_DYNAREC_CALLRET", "1"),
                ("BOX64_DYNAREC_NATIVEX86", "1"),
                (
                    "BOX64_DYNAREC_PROGRAMS",
                    "PD2Launcher.exe:STRONGMEM:WAIT;UpdateUtility.exe:STRONGMEM:WAIT;\
                     Game.exe:CALLRET:NATIVEX86;"
                ),
                ("BOX64_DYNAREC_STRONGMEM", "1"),
                ("BOX64_DYNAREC_WAIT", "0"),
            ])
        );
        assert_eq!(
            environment(Run::OfficialLauncher {
                game_strict_memory_order: true
            }),
            pairs(&[
                ("BOX64_DYNAREC_CALLRET", "1"),
                ("BOX64_DYNAREC_NATIVEX86", "1"),
                (
                    "BOX64_DYNAREC_PROGRAMS",
                    "PD2Launcher.exe:STRONGMEM:WAIT;UpdateUtility.exe:STRONGMEM:WAIT;\
                     Game.exe:CALLRET:NATIVEX86:STRONGMEM;"
                ),
                ("BOX64_DYNAREC_STRONGMEM", "1"),
                ("BOX64_DYNAREC_WAIT", "0"),
            ])
        );
    }

    #[test]
    fn a_game_that_the_official_launcher_starts_gets_what_play_gives_it() {
        for strict in [false, true] {
            let play = environment(Run::Game {
                strict_memory_order: strict,
            });
            let launcher = environment(Run::OfficialLauncher {
                game_strict_memory_order: strict,
            });
            // Play's settings for the game, the program list aside.
            let play = play
                .into_iter()
                .filter(|(name, _)| name != "BOX64_DYNAREC_PROGRAMS")
                .collect::<Vec<_>>();
            // The game's entry names them all, and nothing else.
            assert_eq!(
                named(
                    Run::OfficialLauncher {
                        game_strict_memory_order: strict
                    },
                    "Game.exe"
                ),
                Some(
                    play.iter()
                        .map(|(name, _)| name.trim_start_matches("BOX64_DYNAREC_").to_owned())
                        .collect::<BTreeSet<_>>()
                ),
                "strict {strict}"
            );
            // And each has Play's value in the launcher's environment.
            for setting in &play {
                assert!(launcher.contains(setting), "strict {strict}: {setting:?}");
            }
        }
    }

    #[test]
    fn the_desktop_gets_no_box64_setting() {
        for run in [
            Run::Installer,
            Run::OfficialLauncher {
                game_strict_memory_order: true,
            },
            Run::Game {
                strict_memory_order: true,
            },
        ] {
            assert!(settings(run, false).is_empty(), "{run:?}");
        }
    }
}
