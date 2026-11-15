//! Wine processes left holding the installation lease after their session
//! died. Every Wine process the Launcher starts inherits the installation
//! lease, so a client that outlives its `wineserver` (under FEX, when
//! gamescope closes the Launcher mid-game) keeps the
//! installation locked forever and no Launcher can start. Such a client can
//! do nothing more without its server, so stopping it is the only recovery.

use std::{
    ffi::OsStr,
    fs, io,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

/// A process seen in `/proc`: its command line and executable, and whether it
/// holds the installation lease file open.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ProcessEvidence {
    pub(crate) pid: u32,
    pub(crate) arguments: Vec<PathBuf>,
    pub(crate) executable: Option<PathBuf>,
    pub(crate) holds_lease: bool,
}

impl ProcessEvidence {
    /// Whether this process runs a program from the managed runtime, either
    /// directly or as an argument of a translator such as FEX.
    fn runs_from(&self, runner: &Path) -> bool {
        self.executable
            .iter()
            .chain(&self.arguments)
            .any(|path| path.starts_with(runner))
    }

    fn is_wineserver(&self, runner: &Path) -> bool {
        self.executable
            .iter()
            .chain(&self.arguments)
            .any(|path| path.starts_with(runner) && path.ends_with("bin/wineserver"))
    }
}

/// Who holds the installation lease, as far as this user's processes show.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum LeaseHolders {
    /// Nobody (or nobody this user can see).
    Free,
    /// Wine processes of the managed runtime with no `wineserver`: a dead
    /// session's leftovers, safe to stop.
    Orphaned(Vec<u32>),
    /// Wine processes of the managed runtime whose server is running: the
    /// game or the official launcher of another session is still open.
    LiveSession,
    /// A holder that is not the managed runtime's Wine: another Launcher, or
    /// a program a Wine session started (a browser opened from the game).
    OtherProgram,
}

/// Classify the lease holders. Only an `Orphaned` set is ever stopped: a live
/// session always has its server, and a holder that is not Wine is never
/// touched.
pub(crate) fn classify_holders(processes: &[ProcessEvidence], runner: &Path) -> LeaseHolders {
    let holders: Vec<&ProcessEvidence> = processes
        .iter()
        .filter(|process| process.holds_lease)
        .collect();
    if holders.is_empty() {
        LeaseHolders::Free
    } else if holders.iter().any(|holder| !holder.runs_from(runner)) {
        LeaseHolders::OtherProgram
    } else if processes
        .iter()
        .any(|process| process.is_wineserver(runner))
    {
        LeaseHolders::LiveSession
    } else {
        LeaseHolders::Orphaned(holders.iter().map(|holder| holder.pid).collect())
    }
}

/// The lease holders to stop, or none (see [`classify_holders`]).
pub(crate) fn orphaned_holders(processes: &[ProcessEvidence], runner: &Path) -> Vec<u32> {
    match classify_holders(processes, runner) {
        LeaseHolders::Orphaned(pids) => pids,
        _ => Vec::new(),
    }
}

/// This user's processes, with which of them hold `lease` open. Processes
/// that exit or cannot be read during the scan are skipped.
fn scan(lease: &Path) -> io::Result<Vec<ProcessEvidence>> {
    let own = std::process::id();
    let mut processes = Vec::new();
    for entry in fs::read_dir("/proc")? {
        let Some(pid) = entry
            .ok()
            .and_then(|entry| entry.file_name().to_str()?.parse::<u32>().ok())
            .filter(|&pid| pid != own)
        else {
            continue;
        };
        let directory = PathBuf::from(format!("/proc/{pid}"));
        let Ok(command_line) = fs::read(directory.join("cmdline")) else {
            continue;
        };
        let arguments = command_line
            .split(|&byte| byte == 0)
            .filter(|argument| !argument.is_empty())
            .map(|argument| {
                resolve_own_descriptor(Path::new(OsStr::from_bytes(argument)), |number| {
                    fs::read_link(directory.join("fd").join(number)).ok()
                })
            })
            .collect();
        let holds_lease = fs::read_dir(directory.join("fd")).is_ok_and(|descriptors| {
            descriptors.flatten().any(|descriptor| {
                fs::read_link(descriptor.path()).is_ok_and(|target| target == lease)
            })
        });
        processes.push(ProcessEvidence {
            pid,
            arguments,
            executable: fs::read_link(directory.join("exe")).ok(),
            holds_lease,
        });
    }
    Ok(processes)
}

/// The path an argument names when it goes through the process's own
/// descriptors: the Launcher starts Wine as `/proc/self/fd/<N>/wine` (and its
/// preloader as `/proc/self/fd/<N>`), which only `/proc/<pid>/fd/<N>` of that
/// process resolves. Any other argument is returned unchanged.
fn resolve_own_descriptor(
    argument: &Path,
    descriptor_target: impl FnOnce(&OsStr) -> Option<PathBuf>,
) -> PathBuf {
    let Ok(rest) = argument.strip_prefix("/proc/self/fd") else {
        return argument.to_path_buf();
    };
    let mut components = rest.components();
    match components
        .next()
        .and_then(|number| descriptor_target(number.as_os_str()))
    {
        Some(target) => target.join(components.as_path()),
        None => argument.to_path_buf(),
    }
}

/// How long the orphaned state must persist before anything is stopped: a
/// Wine client briefly holds the lease before it has started its server.
const SETTLE: Duration = Duration::from_secs(2);
/// How long to wait for stopped holders to release the lease.
const RELEASE_TIMEOUT: Duration = Duration::from_secs(5);

/// What [`release_orphaned_wine`] found and did.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LeaseRelease {
    /// Nothing held it any more (or nothing this user can see).
    Free,
    /// This many orphaned Wine processes were stopped.
    Stopped(usize),
    /// The game or the official launcher of another session is still open.
    LiveSession,
    /// Another Launcher, or another program, holds it.
    OtherProgram,
}

/// Stop Wine processes of the managed runtime at `managed_root` that hold its
/// installation lease without a `wineserver`, if the same holders are still
/// orphaned after a short wait, and say who else holds it otherwise.
pub(crate) fn release_orphaned_wine(managed_root: &Path) -> io::Result<LeaseRelease> {
    let lease = fs::canonicalize(managed_root.join(".pd2launcher.lock"))?;
    let runner = fs::canonicalize(managed_root.join("runner"))?;
    let orphans = match classify_holders(&scan(&lease)?, &runner) {
        LeaseHolders::Orphaned(pids) => pids,
        LeaseHolders::Free => return Ok(LeaseRelease::Free),
        LeaseHolders::LiveSession => return Ok(LeaseRelease::LiveSession),
        LeaseHolders::OtherProgram => return Ok(LeaseRelease::OtherProgram),
    };
    thread::sleep(SETTLE);
    let confirmed: Vec<u32> = orphaned_holders(&scan(&lease)?, &runner)
        .into_iter()
        .filter(|pid| orphans.contains(pid))
        .collect();
    for &pid in &confirmed {
        if let Some(pid) = rustix::process::Pid::from_raw(pid as i32) {
            let _ = rustix::process::kill_process(pid, rustix::process::Signal::Kill);
        }
    }
    let deadline = Instant::now() + RELEASE_TIMEOUT;
    // A failed scan ends the wait; the holders were stopped either way.
    while Instant::now() < deadline
        && scan(&lease).is_ok_and(|processes| {
            processes
                .iter()
                .any(|process| process.holds_lease && confirmed.contains(&process.pid))
        })
    {
        thread::sleep(Duration::from_millis(100));
    }
    Ok(if confirmed.is_empty() {
        LeaseRelease::Free
    } else {
        LeaseRelease::Stopped(confirmed.len())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const RUNNER: &str = "/home/user/.pd2launcher/game/runner";

    fn process(
        pid: u32,
        arguments: &[&str],
        executable: &str,
        holds_lease: bool,
    ) -> ProcessEvidence {
        ProcessEvidence {
            pid,
            arguments: arguments.iter().map(PathBuf::from).collect(),
            executable: Some(PathBuf::from(executable)),
            holds_lease,
        }
    }

    fn fex_wine(pid: u32, program: &str, holds_lease: bool) -> ProcessEvidence {
        process(
            pid,
            &[
                "/run/rocknix-fex/bin/FEX",
                &format!("{RUNNER}/lutris-GE-Proton8-26-x86_64/bin/wine64-preloader"),
                program,
            ],
            "/run/rocknix-fex/bin/FEX",
            holds_lease,
        )
    }

    #[test]
    fn wine_clients_holding_the_lease_without_a_wineserver_are_orphans() {
        let processes = [
            fex_wine(131_359, r"C:\windows\system32\winedevice.exe", true),
            process(127, &["thunar"], "/usr/bin/thunar", false),
        ];
        assert_eq!(orphaned_holders(&processes, Path::new(RUNNER)), [131_359]);
        // Native Wine renames its command line; its executable still shows.
        let native = process(
            7,
            &[r"C:\windows\system32\winedevice.exe"],
            &format!("{RUNNER}/wine-11.11/bin/wine64-preloader"),
            true,
        );
        assert_eq!(orphaned_holders(&[native], Path::new(RUNNER)), [7]);
    }

    #[test]
    fn programs_started_through_the_launchers_own_descriptors_resolve_to_the_runtime() {
        let bin = PathBuf::from(format!("{RUNNER}/lutris-GE-Proton8-26-x86_64/bin"));
        let target = |number: &OsStr| (number == "233").then(|| bin.clone());
        assert_eq!(
            resolve_own_descriptor(Path::new("/proc/self/fd/233/wine"), target),
            bin.join("wine")
        );
        assert_eq!(
            resolve_own_descriptor(Path::new("/proc/self/fd/233"), target),
            bin
        );
        // Unknown descriptors and ordinary arguments stay as they are.
        assert_eq!(
            resolve_own_descriptor(Path::new("/proc/self/fd/9/wine"), target),
            Path::new("/proc/self/fd/9/wine")
        );
        assert_eq!(
            resolve_own_descriptor(Path::new("Game.exe"), target),
            Path::new("Game.exe")
        );
        // So Game.exe started by the Launcher under FEX counts as its Wine.
        let game = process(
            20,
            &[
                "/run/rocknix-fex/bin/FEX",
                &format!("{RUNNER}/lutris-GE-Proton8-26-x86_64/bin/wine-preloader"),
                &format!("{RUNNER}/lutris-GE-Proton8-26-x86_64/bin/wine"),
                "Game.exe",
            ],
            "/run/rocknix-fex/bin/FEX",
            true,
        );
        assert_eq!(orphaned_holders(&[game], Path::new(RUNNER)), [20]);
    }

    #[test]
    fn a_live_session_another_launcher_or_a_free_lease_is_left_alone() {
        let runner = Path::new(RUNNER);
        let client = fex_wine(10, r"C:\Diablo II\Game.exe", true);
        let server = process(
            11,
            &[
                "/run/rocknix-fex/bin/FEX",
                &format!("{RUNNER}/lutris-GE-Proton8-26-x86_64/bin/wineserver"),
            ],
            "/run/rocknix-fex/bin/FEX",
            false,
        );
        // Its wineserver is running, even without the lease open.
        assert_eq!(
            classify_holders(&[client.clone(), server.clone()], runner),
            LeaseHolders::LiveSession
        );
        assert!(orphaned_holders(&[client.clone(), server], runner).is_empty());
        // Another Launcher holds the lease alongside Wine.
        let launcher = process(
            12,
            &["/home/user/pd2-launcher"],
            "/home/user/pd2-launcher",
            true,
        );
        assert_eq!(
            classify_holders(&[client.clone(), launcher.clone()], runner),
            LeaseHolders::OtherProgram
        );
        assert!(orphaned_holders(&[client.clone(), launcher], runner).is_empty());
        // Wine from some other runtime is not the Launcher's.
        let foreign = process(
            13,
            &["/usr/bin/wine64-preloader"],
            "/usr/bin/wine64-preloader",
            true,
        );
        assert!(orphaned_holders(&[foreign], runner).is_empty());
        // Nobody holds the lease.
        assert_eq!(
            classify_holders(&[fex_wine(14, "x.exe", false)], runner),
            LeaseHolders::Free
        );
    }
}
