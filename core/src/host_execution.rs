//! CPU-neutral Host capability and process-execution interface.
//!
//! Concrete adapters for a Supported host row implement this seam without
//! leaking FEX, Wine, CPU architecture, or absolute host paths into domain
//! policy. Workflows describe a guest ELF with [`ProcessRequest::guest`]; the
//! [`GuestExecutor`] chosen once by composition decides whether that guest is
//! translated (AArch64/FEX) or executed directly (x86-64).

use std::{
    collections::HashMap,
    env,
    ffi::{OsStr, OsString},
    fmt, fs,
    fs::File,
    io::{self, Read},
    os::fd::{AsFd, AsRawFd, OwnedFd},
    os::unix::fs::{FileExt, FileTypeExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
    sync::{Arc, Condvar, Mutex},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use command_fds::{CommandFdExt, FdMapping};

use crate::{
    audit::{
        DisplaySession, GraphicsExecutionEvidence, GuestExecutionBlocker, GuestExecutionEvidence,
        HostEvidence, HostPlatform, HostProbe, Ia32Emulation, RuntimeExecutionEvidence,
        RuntimePreflightRequest, WineLibrary,
    },
    control_channel::HostFacts,
};

const HOST_COMMAND_TIMEOUT: Duration = Duration::from_secs(5);
/// FEX's first run starts FEXServer and loads the RootFS, which can take
/// several seconds on a slow device.
const TRANSLATOR_PROBE_TIMEOUT: Duration = Duration::from_secs(30);
/// A dynamically linked program every x86-64 RootFS has. FEX resolves the
/// path inside its RootFS, so it runs only when a RootFS is usable.
const TRANSLATOR_PROBE_PROGRAM: &str = "/usr/bin/true";
const HOST_COMMAND_OUTPUT_LIMIT: u64 = 4096;
const PROCESS_DRAIN_GRACE: Duration = Duration::from_millis(50);
const HOST_FILE_LIMIT: u64 = 64 * 1024;
const LD_SO_CACHE_LIMIT: u64 = 16 * 1024 * 1024;
const LD_SO_CACHE_ENTRY_LIMIT: usize = 65_536;
const VULKAN_ICD_SEARCH_LIMIT: usize = 32;

/// Launcher environment names forwarded to a translated guest, and only when
/// the Launcher itself has them set.
const TRANSLATOR_PASSTHROUGH_ENVIRONMENT: [&str; 1] = ["FEX_ROOTFS"];
/// binfmt_misc registrations whose `interpreter` line names FEX.
const FEX_BINFMT_ENTRIES: [&str; 2] = ["FEX-x86_64", "FEX-x86"];
/// Fixed FEX locations. `$PATH` is never consulted.
const FEX_CANDIDATES: [&str; 4] = [
    "/usr/bin/FEX",
    "/usr/bin/FEXInterpreter",
    "/usr/local/bin/FEX",
    "/usr/local/bin/FEXInterpreter",
];
/// DroidDeck's FEX wrapper (its Linux desktop on Android): it finds the FEX
/// Steam installed and the Steam Linux Runtime it uses as the RootFS, and
/// runs a program with `run --mode on -- <program> <arguments>`.
const DROIDDECK_FEX: &str = "/usr/local/bin/droiddeck-fex";
const DROIDDECK_FEX_ARGUMENTS: [&str; 4] = ["run", "--mode", "on", "--"];

/// Read-only Linux probe used by the native composition root.
///
/// It reads system identity and file metadata, executes RPM's query operation,
/// and may run the already-validated Managed runtime version preflight through
/// the same [`GuestExecutor`] that Host execution uses. It never invokes a
/// package-manager mutation or an unvalidated executable.
#[derive(Clone, Debug)]
pub struct SystemHostProbe {
    graphics_execution: Option<GraphicsExecutionEvidence>,
    guest: GuestExecutor,
    /// The Android host facts the app handed to the Launcher engine.
    android: Option<HostFacts>,
}

impl SystemHostProbe {
    pub fn new(
        graphics_execution: Option<GraphicsExecutionEvidence>,
        guest: GuestExecutor,
    ) -> Self {
        Self {
            graphics_execution,
            guest,
            android: None,
        }
    }

    /// An Android host: its facts replace `os-release`, which Android does
    /// not have.
    pub(crate) fn with_android_host(mut self, facts: HostFacts) -> Self {
        self.android = Some(facts);
        self
    }

    pub(crate) fn android_host(&self) -> Option<&HostFacts> {
        self.android.as_ref()
    }
}

impl HostProbe for SystemHostProbe {
    fn probe(&self, runtime: &RuntimePreflightRequest) -> io::Result<HostEvidence> {
        let (os_id, os_version, os_variant, kernel_release) = match &self.android {
            Some(facts) => (
                "android".to_owned(),
                facts.sdk.to_string(),
                Some(facts.device_model.clone()),
                facts.kernel_release.clone(),
            ),
            None => {
                // os-release(5): /etc/os-release is normally a symlink to
                // /usr/lib/os-release, which bounded no-follow reads then use.
                let os_release = match read_bounded(Path::new("/etc/os-release"), HOST_FILE_LIMIT) {
                    Err(error)
                        if error.kind() == io::ErrorKind::NotFound
                            || error.raw_os_error()
                                == Some(rustix::io::Errno::LOOP.raw_os_error()) =>
                    {
                        read_bounded(Path::new("/usr/lib/os-release"), HOST_FILE_LIMIT)?
                    }
                    result => result?,
                };
                let mut values = parse_os_release(&os_release);
                let kernel_release =
                    read_bounded(Path::new("/proc/sys/kernel/osrelease"), HOST_FILE_LIMIT)?;
                (
                    values.remove("ID").unwrap_or_default(),
                    values.remove("VERSION_ID").unwrap_or_default(),
                    values.remove("VARIANT_ID"),
                    kernel_release.trim().to_owned(),
                )
            }
        };
        let display_session = classify_display_session(
            env::var("XDG_SESSION_TYPE").ok().as_deref(),
            env::var_os("WAYLAND_DISPLAY").is_some_and(|value| !value.is_empty()),
            env::var_os("DISPLAY").is_some_and(|value| !value.is_empty()),
        );
        let platform = HostPlatform::current();
        let resolved = self.guest.resolved();
        let guest = &resolved.executor;
        let guest_execution = match platform {
            // The app's ARM64 Wine runs every guest directly, on 4 KB pages.
            _ if self.android.is_some() => GuestExecutionEvidence::AppProvided {
                page_size_4k: rustix::param::page_size() == 4096,
            },
            HostPlatform::Aarch64 => {
                let page_size_4k = rustix::param::page_size() == 4096;
                if resolved.pending {
                    // The chosen built-in FEX, and its RootFS, come with the
                    // Download step; nothing on this device is needed (the
                    // device's FEX, if any, runs only until then).
                    GuestExecutionEvidence::Translated {
                        interpreter_present: true,
                        page_size_4k,
                        rootfs_ready: page_size_4k,
                        pending: true,
                    }
                } else {
                    let interpreter_present = guest
                        .interpreter()
                        .is_some_and(|interpreter| is_usable_executable(&interpreter));
                    // FEX refuses other page sizes, which is its own finding.
                    let rootfs_ready = page_size_4k && translator_rootfs_ready(guest);
                    GuestExecutionEvidence::Translated {
                        interpreter_present,
                        page_size_4k,
                        rootfs_ready,
                        pending: false,
                    }
                }
            }
            HostPlatform::X86_64 => native_guest_evidence(),
            HostPlatform::Other(_) => GuestExecutionEvidence::Unavailable,
        };

        Ok(HostEvidence {
            os_id,
            os_version,
            os_variant,
            architecture: std::env::consts::ARCH.to_owned(),
            kernel_machine: rustix::system::uname()
                .machine()
                .to_string_lossy()
                .into_owned(),
            kernel_release,
            display_session,
            guest_execution,
            // Package-manager metadata is diagnostic context only. The
            // executable capability is authoritative on immutable and
            // non-RPM Linux images.
            fex_package: match platform {
                HostPlatform::Aarch64 if self.android.is_none() => {
                    query_fex_package().ok().flatten()
                }
                _ => None,
            },
            vulkan_loader: vulkan_loader_present(),
            vulkan_driver: vulkan_driver_present(),
            graphics_execution: self.graphics_execution.clone(),
            runtime_execution_preflight: runtime_execution_preflight(runtime, &self.guest),
        })
    }
}

/// How the pinned x86 Wine guest is started on this host row.
///
/// Composition selects one value from the compile target and shares it between
/// the host probe's runtime preflight and every guest [`ProcessRequest`].
#[derive(Clone, Eq, PartialEq)]
pub enum GuestExecutor {
    /// Prepend a translator such as FEX and forward only its named settings.
    Translated {
        interpreter: PathBuf,
        /// Arguments before the guest program (DroidDeck's wrapper takes
        /// `run --mode on --`).
        interpreter_arguments: Vec<OsString>,
        passthrough_environment: Vec<(OsString, OsString)>,
    },
    /// Execute the guest ELF directly (native x86-64 with multilib).
    Direct,
    /// No accepted way to execute the guest exists; guest spawns fail closed.
    Unavailable(GuestExecutorUnavailable),
    /// ARM64: the built-in FEX once the player chose it and it is installed,
    /// otherwise the device's. Looked up for every start, so a
    /// choice or a finished download counts at once.
    Chosen(FexChoice),
}

/// The FEX the player chose in step 1 or Settings, and the device FEX used
/// until the built-in one is chosen and installed.
#[derive(Clone)]
pub struct FexChoice {
    paths: crate::persistence::LauncherPaths,
    device: Box<GuestExecutor>,
}

impl PartialEq for FexChoice {
    fn eq(&self, other: &Self) -> bool {
        self.paths.settings_file() == other.paths.settings_file() && self.device == other.device
    }
}

impl Eq for FexChoice {}

/// The executor a start uses, and whether its FEX or RootFS is still to be
/// installed (by the Download step): the chosen built-in FEX.
struct Resolved {
    executor: GuestExecutor,
    pending: bool,
}

/// Why composition could not select a guest executor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GuestExecutorUnavailable {
    TranslatorMissing,
    ArchitectureUnsupported,
}

/// Why a direct guest start was refused before `exec`.
///
/// glibc's `execvp` retries an `ENOEXEC` image through `/bin/sh`, so an image
/// the kernel cannot execute would otherwise be interpreted as a shell script.
/// Direct execution therefore verifies the exact descriptor first.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DirectGuestRefusal {
    /// Not a readable little-endian i386 or x86-64 ELF regular file.
    UnrecognizedImage,
    /// An ELFCLASS32 guest on a kernel that disables IA-32 emulation.
    Ia32ExecutionDisabled,
}

impl fmt::Display for DirectGuestRefusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::UnrecognizedImage => "direct guest is not an i386 or x86-64 ELF executable",
            Self::Ia32ExecutionDisabled => "IA-32 execution is disabled for the i386 guest",
        })
    }
}

impl std::error::Error for DirectGuestRefusal {}

impl fmt::Debug for GuestExecutor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Translated {
                interpreter,
                interpreter_arguments,
                passthrough_environment,
            } => formatter
                .debug_struct("Translated")
                .field("interpreter", interpreter)
                .field("interpreter_arguments", interpreter_arguments)
                .field(
                    "passthrough_environment_names",
                    &passthrough_environment
                        .iter()
                        .map(|(name, _)| name)
                        .collect::<Vec<_>>(),
                )
                .finish(),
            Self::Direct => formatter.write_str("Direct"),
            Self::Unavailable(reason) => {
                formatter.debug_tuple("Unavailable").field(reason).finish()
            }
            Self::Chosen(choice) => formatter
                .debug_struct("Chosen")
                .field("settings", &choice.paths.settings_file())
                .field("device", &choice.device)
                .finish(),
        }
    }
}

/// Program, argv, and additional environment for one guest start.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct GuestInvocation {
    pub(crate) program: PathBuf,
    pub(crate) arguments: Vec<OsString>,
    pub(crate) environment: Vec<(OsString, OsString)>,
}

impl GuestExecutor {
    /// Select the executor for the running binary's compile target. On
    /// ARM64, `fex` is the FEX the player set in Settings; it is used when it
    /// is an executable file, before any FEX found by itself.
    pub fn discover(fex: Option<&Path>) -> Self {
        let configured = fex.filter(|fex| is_usable_executable(fex));
        let executor = Self::select(
            &HostPlatform::current(),
            || {
                configured.map(Path::to_owned).or_else(|| {
                    discover_translator(
                        Path::new("/proc/sys/fs/binfmt_misc"),
                        &FEX_CANDIDATES,
                        is_usable_executable,
                    )
                })
            },
            |name| env::var_os(name),
        );
        match executor {
            Self::Unavailable(GuestExecutorUnavailable::TranslatorMissing)
                if is_usable_executable(Path::new(DROIDDECK_FEX)) =>
            {
                Self::translated(DROIDDECK_FEX, std::iter::empty())
            }
            executor => executor,
        }
    }

    /// A translated executor with an explicit interpreter. Only the accepted
    /// passthrough names are retained.
    pub fn translated(
        interpreter: impl Into<PathBuf>,
        environment: impl IntoIterator<Item = (OsString, OsString)>,
    ) -> Self {
        let interpreter = interpreter.into();
        // DroidDeck's wrapper takes `run --mode on --` before the guest,
        // whether it was found or set in Settings.
        let interpreter_arguments = if is_droiddeck_fex(&interpreter) {
            DROIDDECK_FEX_ARGUMENTS.map(OsString::from).to_vec()
        } else {
            Vec::new()
        };
        Self::Translated {
            interpreter,
            interpreter_arguments,
            passthrough_environment: environment
                .into_iter()
                .filter(|(name, _)| {
                    TRANSLATOR_PASSTHROUGH_ENVIRONMENT
                        .iter()
                        .any(|accepted| name == accepted)
                })
                .collect(),
        }
    }

    fn select(
        platform: &HostPlatform,
        translator: impl FnOnce() -> Option<PathBuf>,
        environment: impl Fn(&str) -> Option<OsString>,
    ) -> Self {
        match platform {
            HostPlatform::Aarch64 => match translator() {
                Some(interpreter) => Self::translated(
                    interpreter,
                    TRANSLATOR_PASSTHROUGH_ENVIRONMENT
                        .iter()
                        .filter_map(|name| environment(name).map(|value| ((*name).into(), value))),
                ),
                None => Self::Unavailable(GuestExecutorUnavailable::TranslatorMissing),
            },
            HostPlatform::X86_64 => Self::Direct,
            HostPlatform::Other(_) => {
                Self::Unavailable(GuestExecutorUnavailable::ArchitectureUnsupported)
            }
        }
    }

    /// DroidDeck's FEX wrapper: its guests are given real paths.
    fn needs_real_paths(&self) -> bool {
        matches!(self, Self::Translated { interpreter, .. } if is_droiddeck_fex(interpreter))
    }

    /// On ARM64, this device executor inside the player's choice of FEX:
    /// the built-in FEX in `paths`' managed root once chosen and installed.
    pub(crate) fn with_choice(self, paths: crate::persistence::LauncherPaths) -> Self {
        match HostPlatform::current() {
            HostPlatform::Aarch64 => Self::Chosen(FexChoice {
                paths,
                device: Box::new(self),
            }),
            _ => self,
        }
    }

    /// The executor this start uses.
    fn resolved(&self) -> Resolved {
        let Self::Chosen(choice) = self else {
            return Resolved {
                executor: self.clone(),
                pending: false,
            };
        };
        let root = choice.paths.managed_root();
        let settings = crate::persistence::SettingsStore::new(choice.paths.settings_file())
            .load()
            .ok();
        // Only ARM64 builds wrap their executor in a choice (`with_choice`).
        let builtin = crate::builtin_fex::chosen(settings.as_ref(), &root);
        if builtin && crate::builtin_fex::installed(&root) {
            let layout = crate::builtin_fex::Layout::new(&root);
            return Resolved {
                executor: Self::Translated {
                    interpreter: layout.interpreter(),
                    interpreter_arguments: Vec::new(),
                    passthrough_environment: layout.environment(),
                },
                pending: false,
            };
        }
        Resolved {
            executor: (*choice.device).clone(),
            pending: builtin,
        }
    }

    pub fn interpreter(&self) -> Option<PathBuf> {
        match self {
            Self::Translated { interpreter, .. } => Some(interpreter.clone()),
            Self::Direct | Self::Unavailable(_) => None,
            Self::Chosen(_) => self.resolved().executor.interpreter(),
        }
    }

    /// Shape a guest ELF and its arguments into the host program to execute.
    /// Descriptor mappings, working directory, and the request environment
    /// are independent of the executor.
    pub(crate) fn shape(
        &self,
        guest: &Path,
        arguments: &[OsString],
    ) -> io::Result<GuestInvocation> {
        match self {
            Self::Translated {
                interpreter,
                interpreter_arguments,
                passthrough_environment,
            } => Ok(GuestInvocation {
                program: interpreter.clone(),
                arguments: interpreter_arguments
                    .iter()
                    .cloned()
                    .chain(std::iter::once(guest.as_os_str().to_owned()))
                    .chain(arguments.iter().cloned())
                    .collect(),
                environment: passthrough_environment.clone(),
            }),
            Self::Direct => Ok(GuestInvocation {
                program: guest.to_owned(),
                arguments: arguments.to_vec(),
                environment: Vec::new(),
            }),
            Self::Unavailable(_) => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "guest execution is unavailable on this host",
            )),
            Self::Chosen(_) => self.resolved().executor.shape(guest, arguments),
        }
    }
}

/// Locate FEX. The first fixed candidate (`/usr/bin/FEX`, the certified
/// ARM64 row's translator) wins; otherwise the binfmt_misc
/// registration's interpreter, then the remaining fixed locations.
fn discover_translator(
    binfmt_directory: &Path,
    candidates: &[&str],
    trusted: impl Fn(&Path) -> bool,
) -> Option<PathBuf> {
    let (preferred, remaining) = candidates
        .split_first()
        .map_or((None, &[][..]), |(first, rest)| (Some(*first), rest));
    preferred
        .map(PathBuf::from)
        .into_iter()
        .chain(
            FEX_BINFMT_ENTRIES
                .iter()
                .filter_map(|entry| {
                    read_bounded(&binfmt_directory.join(entry), HOST_FILE_LIMIT).ok()
                })
                .filter_map(|registration| parse_binfmt_interpreter(&registration)),
        )
        .chain(remaining.iter().map(PathBuf::from))
        .find(|candidate| trusted(candidate))
}

fn parse_binfmt_interpreter(registration: &str) -> Option<PathBuf> {
    registration.lines().find_map(|line| {
        let path = Path::new(line.strip_prefix("interpreter ")?.trim());
        path.is_absolute().then(|| path.to_owned())
    })
}

/// A regular executable file at an absolute path, links followed. The
/// Launcher does not control the computer it runs on, so it makes no rule
/// about who owns a translator or the directories it is in.
fn is_usable_executable(path: &Path) -> bool {
    path.is_absolute()
        && fs::metadata(path)
            .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

/// An executable and its arguments, kept separate from shell syntax.
pub struct ProcessRequest {
    executable: PathBuf,
    guest: bool,
    arguments: Vec<OsString>,
    working_directory: Option<PathBuf>,
    working_directory_handle: Option<File>,
    clear_environment: bool,
    environment: Vec<(OsString, OsString)>,
    output_limit: Option<usize>,
    /// Detailed logging for this program: its output lines go to standard
    /// error longer ([`crate::activity::live_output`]).
    detailed_output: bool,
    inherited_files: Vec<InheritedFile>,
}

impl fmt::Debug for ProcessRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProcessRequest")
            .field("executable", &"[REDACTED]")
            .field("guest", &self.guest)
            .field("argument_count", &self.arguments.len())
            .field("has_working_directory", &self.working_directory.is_some())
            .field("clear_environment", &self.clear_environment)
            .field("environment_name_count", &self.environment.len())
            .field("output_limit", &self.output_limit)
            .field("detailed_output", &self.detailed_output)
            .field(
                "inherited_file_descriptors",
                &self.inherited_file_descriptors().collect::<Vec<_>>(),
            )
            .finish()
    }
}

impl ProcessRequest {
    pub fn new(executable: impl Into<PathBuf>) -> Self {
        Self {
            executable: executable.into(),
            guest: false,
            arguments: Vec::new(),
            working_directory: None,
            working_directory_handle: None,
            clear_environment: false,
            environment: Vec::new(),
            output_limit: None,
            detailed_output: false,
            inherited_files: Vec::new(),
        }
    }

    /// Describe a guest (x86 Wine) ELF, normally an inherited
    /// `/proc/self/fd/N` path. The CPU-neutral request never names a
    /// translator; the adapter's [`GuestExecutor`] shapes the host argv.
    pub fn guest(executable: impl Into<PathBuf>) -> Self {
        Self {
            guest: true,
            ..Self::new(executable)
        }
    }

    pub fn with_argument(mut self, argument: impl Into<OsString>) -> Self {
        self.arguments.push(argument.into());
        self
    }

    pub fn with_arguments(
        mut self,
        arguments: impl IntoIterator<Item = impl Into<OsString>>,
    ) -> Self {
        self.arguments.extend(arguments.into_iter().map(Into::into));
        self
    }

    /// Retain the exact directory object used for child `chdir` while keeping
    /// the display path available to typed tests and diagnostics.
    pub(crate) fn with_working_directory_handle(
        mut self,
        directory: impl Into<PathBuf>,
        handle: File,
    ) -> Self {
        self.working_directory = Some(directory.into());
        self.working_directory_handle = Some(handle);
        self
    }

    /// Start with an empty environment before applying typed profile values.
    pub fn with_cleared_environment(mut self) -> Self {
        self.clear_environment = true;
        self
    }

    pub fn with_environment(
        mut self,
        name: impl Into<OsString>,
        value: impl Into<OsString>,
    ) -> Self {
        self.environment.push((name.into(), value.into()));
        self
    }

    /// Capture each stream and drain it continuously, keeping at most this
    /// many bytes of it: all of it when it fits, else its first and its last
    /// half with a line between them that says how much is left out
    /// ([`ProcessOutput::stdout`] is then a little longer than the bound).
    pub fn with_output_limit(mut self, bytes_per_stream: usize) -> Self {
        self.output_limit = Some(bytes_per_stream);
        self
    }

    /// With Detailed logging, the program's output lines go to standard error
    /// up to 1000 characters, not 300 ([`crate::activity::live_output`]).
    pub fn with_detailed_output(mut self, detailed: bool) -> Self {
        self.detailed_output = detailed;
        self
    }

    /// Make an already-open Launcher-owned resource survive into the child.
    ///
    /// This is used for lifetime leases, not for executable selection. The
    /// descriptor number must be unique and outside the standard streams.
    pub fn with_inherited_file(mut self, file: File, child_descriptor: i32) -> io::Result<Self> {
        if child_descriptor < 3
            || self
                .inherited_files
                .iter()
                .any(|inherited| inherited.child_descriptor == child_descriptor)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "inherited descriptor must be unique and greater than two",
            ));
        }
        self.inherited_files.push(InheritedFile {
            file,
            child_descriptor,
        });
        Ok(self)
    }

    pub fn executable(&self) -> &Path {
        &self.executable
    }

    /// Whether `executable` is a guest ELF shaped by the host's executor.
    pub fn is_guest(&self) -> bool {
        self.guest
    }

    pub fn arguments(&self) -> &[OsString] {
        &self.arguments
    }

    pub fn working_directory(&self) -> Option<&Path> {
        self.working_directory.as_deref()
    }

    pub fn clears_environment(&self) -> bool {
        self.clear_environment
    }

    pub fn environment(&self) -> &[(OsString, OsString)] {
        &self.environment
    }

    pub fn output_limit(&self) -> Option<usize> {
        self.output_limit
    }

    pub fn inherited_file_descriptors(&self) -> impl Iterator<Item = i32> + '_ {
        self.inherited_files
            .iter()
            .map(|inherited| inherited.child_descriptor)
    }

    #[cfg(test)]
    pub(crate) fn clone_inherited_file(&self, child_descriptor: i32) -> io::Result<File> {
        self.inherited_files
            .iter()
            .find(|inherited| inherited.child_descriptor == child_descriptor)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "descriptor was not inherited"))?
            .file
            .try_clone()
    }
}

struct InheritedFile {
    file: File,
    child_descriptor: i32,
}

/// CPU-neutral entry point for starting supervised processes.
pub trait HostExecution: Send + Sync {
    fn spawn(&self, request: ProcessRequest) -> io::Result<Box<dyn SupervisedProcess>>;
}

/// A child process whose lifetime remains owned by a launcher workflow.
pub trait SupervisedProcess: Send {
    fn try_wait(&mut self) -> io::Result<Option<ProcessExit>>;
    fn terminate(&mut self) -> io::Result<()>;

    /// Reap the owned child after termination. Workflows use this before
    /// releasing mutation leases, so cancellation never detaches a process.
    fn wait(&mut self) -> io::Result<ProcessExit> {
        loop {
            if let Some(exit) = self.try_wait()? {
                return Ok(exit);
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    /// Return the bounded output captured so far. Implementations that do not
    /// capture output may retain this safe empty default.
    fn output(&self) -> ProcessOutput {
        ProcessOutput::default()
    }

    /// The activity log's name for this process when it is the UI helper
    /// supervised beside another program.
    fn helper_label(&self) -> &'static str {
        "installer UI helper"
    }
}

/// Portable process-completion evidence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProcessExit {
    success: bool,
    code: Option<i32>,
}

impl ProcessExit {
    pub fn new(success: bool, code: Option<i32>) -> Self {
        Self { success, code }
    }

    pub fn success(self) -> bool {
        self.success
    }

    pub fn code(self) -> Option<i32> {
        self.code
    }
}

/// Bounded child output. Truncation is explicit and no command/environment is
/// rendered as part of this record.
#[derive(Clone, Default, Eq, PartialEq)]
pub struct ProcessOutput {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    stdout_truncated: bool,
    stderr_truncated: bool,
}

impl fmt::Debug for ProcessOutput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProcessOutput")
            .field("stdout_bytes", &self.stdout.len())
            .field("stderr_bytes", &self.stderr.len())
            .field("stdout_truncated", &self.stdout_truncated)
            .field("stderr_truncated", &self.stderr_truncated)
            .finish()
    }
}

impl ProcessOutput {
    pub fn new(
        stdout: Vec<u8>,
        stderr: Vec<u8>,
        stdout_truncated: bool,
        stderr_truncated: bool,
    ) -> Self {
        Self {
            stdout,
            stderr,
            stdout_truncated,
            stderr_truncated,
        }
    }

    pub fn stdout(&self) -> &[u8] {
        &self.stdout
    }

    pub fn stderr(&self) -> &[u8] {
        &self.stderr
    }

    pub fn stdout_truncated(&self) -> bool {
        self.stdout_truncated
    }

    pub fn stderr_truncated(&self) -> bool {
        self.stderr_truncated
    }
}

/// Native adapter for a typed, shell-free process request.
#[derive(Clone, Debug)]
pub struct SystemHostExecution {
    guest: GuestExecutor,
}

impl SystemHostExecution {
    pub fn new(guest: GuestExecutor) -> Self {
        Self { guest }
    }

    /// The translator guest programs are started with, if any.
    pub fn interpreter(&self) -> Option<PathBuf> {
        self.guest.interpreter()
    }
}

impl HostExecution for SystemHostExecution {
    fn spawn(&self, request: ProcessRequest) -> io::Result<Box<dyn SupervisedProcess>> {
        let guest = &self.guest.resolved().executor;
        if request.guest && matches!(guest, GuestExecutor::Direct) {
            direct_guest_check(&request, current_ia32_emulation())
                .map_err(|refusal| io::Error::new(io::ErrorKind::Unsupported, refusal))?;
        }
        let invocation = if request.guest && guest.needs_real_paths() {
            // Under DroidDeck's FEX a guest cannot resolve `/proc/self/fd/N`
            // to the file behind it (Wine finds its libraries beside its
            // own real path), so it is given the real paths instead.
            let real = real_descriptor_paths(&request.inherited_files);
            let mut invocation = guest.shape(
                Path::new(&real_path(request.executable.as_os_str(), &real)),
                &request
                    .arguments
                    .iter()
                    .map(|argument| real_path(argument, &real))
                    .collect::<Vec<_>>(),
            )?;
            invocation.environment.extend(
                request
                    .environment
                    .iter()
                    .filter(|(_, value)| {
                        std::os::unix::ffi::OsStrExt::as_bytes(value.as_os_str())
                            .starts_with(b"/proc/self/fd/")
                    })
                    .map(|(name, value)| (name.clone(), real_path(value, &real))),
            );
            // Wine finds ntdll.so beside its own program through
            // /proc/self/exe, which under DroidDeck's FEX names FEX; its
            // libraries are named for it instead.
            let guest = real_path(request.executable.as_os_str(), &real);
            if let Some(dll_path) = wine_dll_path(
                Path::new(&guest),
                request
                    .environment
                    .iter()
                    .find(|(name, _)| name == "WINEDLLPATH")
                    .map(|(_, value)| value.as_os_str()),
            ) {
                invocation
                    .environment
                    .push((OsString::from("WINEDLLPATH"), dll_path));
            }
            invocation
        } else if request.guest {
            guest.shape(&request.executable, &request.arguments)?
        } else {
            GuestInvocation {
                program: request.executable.clone(),
                arguments: request.arguments.clone(),
                environment: Vec::new(),
            }
        };
        let mut command = Command::new(&invocation.program);
        command.args(&invocation.arguments).stdin(Stdio::null());
        if let Some(directory) = &request.working_directory_handle {
            command.current_dir(format!("/proc/self/fd/{}", directory.as_raw_fd()));
        }
        if request.clear_environment {
            command.env_clear();
        }
        command.envs(request.environment.iter().cloned());
        command.envs(invocation.environment);
        if request.guest && matches!(guest, GuestExecutor::Translated { .. }) {
            // The host's own graphics-driver paths and glibc tuning name
            // ARM64 files and settings; the x86 guest under FEX finds its
            // own in FEX's RootFS.
            for name in TRANSLATED_GUEST_WITHHELD {
                command.env_remove(name);
            }
        }

        let captures = request.output_limit.map(|limit| {
            command.stdout(Stdio::piped()).stderr(Stdio::piped());
            (
                Arc::new(Mutex::new(BoundedCapture::new(limit))),
                Arc::new(Mutex::new(BoundedCapture::new(limit))),
            )
        });
        if captures.is_none() {
            command.stdout(Stdio::null()).stderr(Stdio::null());
        }

        let mappings = request
            .inherited_files
            .into_iter()
            .map(|inherited| FdMapping {
                parent_fd: OwnedFd::from(inherited.file),
                child_fd: inherited.child_descriptor,
            })
            .collect::<Vec<_>>();
        if !mappings.is_empty() {
            command.fd_mappings(mappings).map_err(|error| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("invalid inherited descriptor mapping: {error}"),
                )
            })?;
        }

        let mut child = command.spawn()?;
        let drain_completion = captures
            .as_ref()
            .map(|_| Arc::new(DrainCompletion::default()));
        if let Some((stdout_capture, stderr_capture)) = &captures {
            let stdout = child
                .stdout
                .take()
                .ok_or_else(|| io::Error::other("child stdout was not piped"))?;
            let stderr = child
                .stderr
                .take()
                .ok_or_else(|| io::Error::other("child stderr was not piped"))?;
            let completion = drain_completion
                .as_ref()
                .expect("capture completion is created with captures");
            let detailed = request.detailed_output;
            drain_stream(
                stdout,
                Arc::clone(stdout_capture),
                Arc::clone(completion),
                detailed,
            );
            drain_stream(
                stderr,
                Arc::clone(stderr_capture),
                Arc::clone(completion),
                detailed,
            );
        }

        Ok(Box::new(SystemSupervisedProcess {
            child,
            captures,
            drain_completion,
        }))
    }
}

/// Verify a direct guest through the object the child will execute: the
/// inherited descriptor named by `/proc/self/fd/N`, or otherwise the path.
fn direct_guest_check(
    request: &ProcessRequest,
    ia32: Ia32Emulation,
) -> Result<(), DirectGuestRefusal> {
    let descriptor = request
        .executable
        .strip_prefix("/proc/self/fd")
        .ok()
        .and_then(|rest| rest.to_str()?.parse::<i32>().ok());
    let refusal = match descriptor {
        Some(descriptor) => request
            .inherited_files
            .iter()
            .find(|inherited| inherited.child_descriptor == descriptor)
            .map_or(Some(DirectGuestRefusal::UnrecognizedImage), |inherited| {
                direct_guest_refusal(&inherited.file, ia32)
            }),
        None => rustix::fs::open(
            &request.executable,
            rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::NONBLOCK | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .map_or(Some(DirectGuestRefusal::UnrecognizedImage), |descriptor| {
            direct_guest_refusal(&File::from(descriptor), ia32)
        }),
    };
    refusal.map_or(Ok(()), Err)
}

/// Host variables a translated guest never gets: they point at the host's
/// own (ARM64) driver files and tune its own glibc.
const TRANSLATED_GUEST_WITHHELD: [&str; 10] = [
    "VK_ICD_FILENAMES",
    "VK_DRIVER_FILES",
    "VK_ADD_DRIVER_FILES",
    "VK_LAYER_PATH",
    "VK_ADD_LAYER_PATH",
    "LIBGL_DRIVERS_PATH",
    "__EGL_VENDOR_LIBRARY_FILENAMES",
    "__EGL_VENDOR_LIBRARY_DIRS",
    "LIBVA_DRIVERS_PATH",
    "GLIBC_TUNABLES",
];

/// DroidDeck's FEX wrapper, at its place or wherever Settings names it.
fn is_droiddeck_fex(interpreter: &Path) -> bool {
    interpreter == Path::new(DROIDDECK_FEX)
        || interpreter.file_name() == Path::new(DROIDDECK_FEX).file_name()
}

/// The real path behind each descriptor a request hands its child, as this
/// process sees it.
fn real_descriptor_paths(inherited: &[InheritedFile]) -> Vec<(i32, PathBuf)> {
    inherited
        .iter()
        .filter_map(|inherited| {
            std::fs::read_link(format!("/proc/self/fd/{}", inherited.file.as_raw_fd()))
                .ok()
                .map(|path| (inherited.child_descriptor, path))
        })
        .collect()
}

/// `WINEDLLPATH` for a Wine whose program is `program` (`<wine>/bin/…`):
/// its own library folders first, then whatever `existing` named.
fn wine_dll_path(program: &Path, existing: Option<&OsStr>) -> Option<OsString> {
    let wine = program
        .parent()
        .filter(|bin| bin.ends_with("bin"))?
        .parent()?;
    let mut folders = ["lib/wine", "lib64/wine"]
        .into_iter()
        .map(|folder| wine.join(folder))
        .filter(|folder| folder.is_dir())
        .map(PathBuf::into_os_string)
        .collect::<Vec<_>>();
    if folders.is_empty() {
        return None;
    }
    folders.extend(
        existing
            .filter(|existing| !existing.is_empty())
            .map(OsStr::to_owned),
    );
    Some(folders.join(OsStr::new(":")))
}

/// `value` with a leading `/proc/self/fd/N` replaced by the real path of `N`.
fn real_path(value: &OsStr, real: &[(i32, PathBuf)]) -> OsString {
    use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
    let bytes = value.as_bytes();
    let joined = |path: &Path, rest: &[u8]| {
        let mut joined = path.as_os_str().as_bytes().to_vec();
        joined.extend_from_slice(rest);
        OsString::from_vec(joined)
    };
    for (descriptor, path) in real {
        let anchor = format!("/proc/self/fd/{descriptor}");
        if let Some(rest) = bytes.strip_prefix(anchor.as_bytes())
            && (rest.is_empty() || rest.starts_with(b"/"))
        {
            return joined(path, rest);
        }
    }
    // Helpers are also named through this process's own descriptors
    // (`/proc/<launcher pid>/fd/N`); one that is a file on disk is named by
    // its path. A sealed in-memory helper has none and is left as it is.
    let own = format!("/proc/{}/fd/", std::process::id());
    if let Some(rest) = bytes.strip_prefix(own.as_bytes()) {
        let end = rest
            .iter()
            .position(|byte| *byte == b'/')
            .unwrap_or(rest.len());
        if let Ok(descriptor) = std::str::from_utf8(&rest[..end])
            .unwrap_or("")
            .parse::<u32>()
            && let Ok(path) = std::fs::read_link(format!("/proc/self/fd/{descriptor}"))
            && path.is_absolute()
            && path.exists()
        {
            return joined(&path, &rest[end..]);
        }
    }
    value.to_owned()
}

/// Read the ELF header at offset zero without moving the shared file offset.
/// An AArch64 ELF (the Android app's ARM64 Wine) runs directly as well.
fn direct_guest_refusal(image: &File, ia32: Ia32Emulation) -> Option<DirectGuestRefusal> {
    let mut header = [0_u8; 20];
    let read = image
        .metadata()
        .is_ok_and(|metadata| metadata.is_file())
        .then(|| image.read_exact_at(&mut header, 0).ok())
        .flatten();
    if cfg!(target_arch = "aarch64") && read.is_some() && is_aarch64_elf(&header) {
        return None;
    }
    let target = read.and_then(|()| elf_target(&header));
    match target {
        None => Some(DirectGuestRefusal::UnrecognizedImage),
        Some(ElfTarget::I386) if ia32 == Ia32Emulation::Disabled => {
            Some(DirectGuestRefusal::Ia32ExecutionDisabled)
        }
        Some(_) => None,
    }
}

type ProcessCaptures = (Arc<Mutex<BoundedCapture>>, Arc<Mutex<BoundedCapture>>);

struct SystemSupervisedProcess {
    child: std::process::Child,
    captures: Option<ProcessCaptures>,
    drain_completion: Option<Arc<DrainCompletion>>,
}

impl SupervisedProcess for SystemSupervisedProcess {
    fn try_wait(&mut self) -> io::Result<Option<ProcessExit>> {
        let status = self.child.try_wait()?;
        if status.is_some()
            && let Some(completion) = &self.drain_completion
        {
            completion.wait_for_streams(PROCESS_DRAIN_GRACE);
        }
        Ok(status.map(|status| ProcessExit::new(status.success(), status.code())))
    }

    fn terminate(&mut self) -> io::Result<()> {
        self.child.kill()
    }

    fn wait(&mut self) -> io::Result<ProcessExit> {
        let status = self.child.wait()?;
        if let Some(completion) = &self.drain_completion {
            completion.wait_for_streams(PROCESS_DRAIN_GRACE);
        }
        Ok(ProcessExit::new(status.success(), status.code()))
    }

    fn output(&self) -> ProcessOutput {
        let Some((stdout, stderr)) = &self.captures else {
            return ProcessOutput::default();
        };
        let stdout = stdout
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let stderr = stderr
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        ProcessOutput::new(
            stdout.contents(),
            stderr.contents(),
            stdout.truncated,
            stderr.truncated,
        )
    }
}

#[derive(Debug, Default)]
struct DrainCompletion {
    streams: Mutex<usize>,
    changed: Condvar,
}

impl DrainCompletion {
    fn complete_stream(&self) {
        let mut streams = self
            .streams
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *streams += 1;
        self.changed.notify_all();
    }

    fn wait_for_streams(&self, timeout: Duration) {
        let streams = self
            .streams
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _ = self
            .changed
            .wait_timeout_while(streams, timeout, |streams| *streams < 2)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
    }
}

/// A program's output, at most `limit` bytes of it: all of it when it fits,
/// else its first half and its last half with a line between them that
/// says how much is left out, so that the record of a long run that failed
/// (the last bytes, [`crate::activity::OUTPUT_TAIL`]) shows how it ended.
#[derive(Debug)]
struct BoundedCapture {
    head: Vec<u8>,
    tail: std::collections::VecDeque<u8>,
    limit: usize,
    left_out: usize,
    truncated: bool,
}

impl BoundedCapture {
    fn new(limit: usize) -> Self {
        Self {
            head: Vec::with_capacity((limit / 2).min(4096)),
            tail: std::collections::VecDeque::new(),
            limit,
            left_out: 0,
            truncated: false,
        }
    }

    fn append(&mut self, bytes: &[u8]) {
        let head_room = (self.limit / 2).saturating_sub(self.head.len());
        let (to_head, rest) = bytes.split_at(head_room.min(bytes.len()));
        self.head.extend_from_slice(to_head);
        let tail_limit = self.limit - self.limit / 2;
        self.tail.extend(rest);
        if self.tail.len() > tail_limit {
            let over = self.tail.len() - tail_limit;
            self.tail.drain(..over);
            self.left_out += over;
            self.truncated = true;
        }
    }

    /// What it holds, in order, with the line about the bytes left out.
    fn contents(&self) -> Vec<u8> {
        let mut contents = self.head.clone();
        if self.left_out > 0 {
            contents.extend_from_slice(
                format!("\n[{} bytes of output left out]\n", self.left_out).as_bytes(),
            );
        }
        contents.extend(&self.tail);
        contents
    }
}

fn drain_stream(
    mut stream: impl Read + Send + 'static,
    capture: Arc<Mutex<BoundedCapture>>,
    completion: Arc<DrainCompletion>,
    detailed: bool,
) {
    thread::spawn(move || {
        let mut buffer = [0_u8; 4096];
        // Complete lines also go to the progress log as they are written.
        let mut line = Vec::new();
        // The capture takes each line with its end, but not Wine's debug
        // lines that the progress log leaves out, and network lines at most
        // as many a second as the progress log has (a failed program's
        // capture then holds its error, not those); a line longer than
        // CAPTURE_LINE goes in as it comes.
        let mut pending = Vec::new();
        let mut passing = false;
        // The `\n` of a `\r\n` whose line was left out.
        let mut dropped_cr = false;
        let mut network = crate::activity::CaptureNetworkLines::default();
        let mut kept_in_capture = |line: &[u8]| match crate::activity::capture_line(line) {
            crate::activity::CaptureLine::Keep => true,
            crate::activity::CaptureLine::Drop => false,
            crate::activity::CaptureLine::Network => network.take(
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_or(0, |since| since.as_secs()),
            ),
        };
        let keep = |bytes: &[u8]| {
            capture
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .append(bytes);
        };
        loop {
            match stream.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) => {
                    for &byte in &buffer[..count] {
                        pending.push(byte);
                        if byte == b'\n' || byte == b'\r' {
                            let kept = if dropped_cr && pending == b"\n" {
                                false
                            } else {
                                passing || kept_in_capture(&pending[..pending.len() - 1])
                            };
                            if kept {
                                keep(&pending);
                            }
                            dropped_cr = !kept && byte == b'\r';
                            pending.clear();
                            passing = false;
                            crate::activity::live_output(&String::from_utf8_lossy(&line), detailed);
                            line.clear();
                        } else {
                            if pending.len() >= CAPTURE_LINE {
                                keep(&pending);
                                pending.clear();
                                passing = true;
                            }
                            if line.len() < 1024 {
                                line.push(byte);
                            }
                        }
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
        if !pending.is_empty() && (passing || kept_in_capture(&pending)) {
            keep(&pending);
        }
        crate::activity::output_ended();
        completion.complete_stream();
    });
}

/// The longest line the capture holds back until its end, to see whether it
/// keeps it ([`drain_stream`]).
const CAPTURE_LINE: usize = 8 * 1024;

fn read_bounded(path: &Path, limit: u64) -> io::Result<String> {
    String::from_utf8(read_bounded_bytes(path, limit)?)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "host file is not UTF-8"))
}

/// Read a host file without following a final symlink and without blocking
/// on a FIFO or device; only a regular file (judged by the open descriptor)
/// is read.
fn read_bounded_bytes(path: &Path, limit: u64) -> io::Result<Vec<u8>> {
    let file = File::from(rustix::fs::open(
        path,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::NONBLOCK
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )?);
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "host file is not a regular file",
        ));
    }
    // procfs and sysfs report zero length, so the size check only rejects
    // oversized regular files early; the bounded read is authoritative.
    if metadata.len() > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "host file exceeds limit",
        ));
    }
    let mut output = Vec::new();
    file.take(limit + 1).read_to_end(&mut output)?;
    if output.len() as u64 > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "host file exceeds limit",
        ));
    }
    Ok(output)
}

/// Parse os-release(5): `KEY=value` assignments with optional shell-style
/// single or double quotes, backslash escapes inside double quotes, and `#`
/// comment lines.
fn parse_os_release(input: &str) -> HashMap<String, String> {
    input
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let (key, value) = line.split_once('=')?;
            if key.is_empty()
                || !key
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
            {
                return None;
            }
            Some((key.to_owned(), unquote_os_release_value(value)?))
        })
        .collect()
}

fn unquote_os_release_value(value: &str) -> Option<String> {
    let mut output = String::new();
    let mut characters = value.chars();
    while let Some(character) = characters.next() {
        match character {
            '"' => loop {
                match characters.next()? {
                    '"' => break,
                    '\\' => match characters.next()? {
                        escaped @ ('"' | '\\' | '$' | '`') => output.push(escaped),
                        other => {
                            output.push('\\');
                            output.push(other);
                        }
                    },
                    other => output.push(other),
                }
            },
            '\'' => loop {
                match characters.next()? {
                    '\'' => break,
                    other => output.push(other),
                }
            },
            '\\' => output.push(characters.next()?),
            character if character.is_whitespace() => break,
            other => output.push(other),
        }
    }
    Some(output)
}

/// A tty-started compositor may leave `XDG_SESSION_TYPE=tty` while exporting
/// a valid Wayland or X11 display, so display presence is also evidence.
fn classify_display_session(
    session_type: Option<&str>,
    wayland_display: bool,
    x11_display: bool,
) -> DisplaySession {
    match session_type.map(str::to_ascii_lowercase).as_deref() {
        Some("wayland") => DisplaySession::Wayland,
        Some("x11") => DisplaySession::X11,
        _ if wayland_display => DisplaySession::Wayland,
        _ if x11_display => DisplaySession::X11,
        None | Some("") => DisplaySession::Unknown,
        Some(_) => DisplaySession::Other,
    }
}

/// The Game profile presents through Wine's `winex11.drv` on both rows
/// (Wine-GE 8 ships no Wayland driver, and the x86-64 WoW64 row keeps the same
/// X11 profile), so the Game needs an X11 (or XWayland) display even inside a
/// Wayland session.
fn x11_display_available(display: Option<&OsStr>, socket_directory: &Path) -> bool {
    let Some(display) = display.and_then(OsStr::to_str) else {
        return false;
    };
    let Some((host, rest)) = display.rsplit_once(':') else {
        return false;
    };
    let number = rest.split('.').next().unwrap_or_default();
    if number.is_empty() || !number.bytes().all(|byte| byte.is_ascii_digit()) {
        return false;
    }
    if host.is_empty() || host == "unix" {
        fs::metadata(socket_directory.join(format!("X{number}")))
            .is_ok_and(|metadata| metadata.file_type().is_socket())
    } else {
        // A remote or TCP display cannot be proven locally; record presence.
        true
    }
}

/// Interpret `ia32_emulation=` the way the kernel does: each occurrence is
/// parsed with `kstrtobool`, an unparsable value leaves the previous state,
/// and the last parsable occurrence wins. Arguments after `--` belong to init.
/// An unreadable command line is `Unknown`, never assumed enabled.
fn kernel_ia32_emulation(command_line: Option<&str>) -> Ia32Emulation {
    let Some(command_line) = command_line else {
        return Ia32Emulation::Unknown;
    };
    let disabled = command_line
        .split_whitespace()
        .take_while(|parameter| *parameter != "--")
        .filter_map(|parameter| parameter.strip_prefix("ia32_emulation="))
        .filter_map(kernel_bool)
        .last()
        == Some(false);
    if disabled {
        Ia32Emulation::Disabled
    } else {
        Ia32Emulation::NotDisabled
    }
}

fn current_ia32_emulation() -> Ia32Emulation {
    kernel_ia32_emulation(
        read_bounded(Path::new("/proc/cmdline"), HOST_FILE_LIMIT)
            .ok()
            .as_deref(),
    )
}

/// The kernel's `kstrtobool`: only the first one or two characters count.
fn kernel_bool(value: &str) -> Option<bool> {
    let mut characters = value.chars();
    match characters.next()? {
        'y' | 'Y' | 't' | 'T' | '1' => Some(true),
        'n' | 'N' | 'f' | 'F' | '0' => Some(false),
        'o' | 'O' => match characters.next()? {
            'n' | 'N' => Some(true),
            'f' | 'F' => Some(false),
            _ => None,
        },
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ElfTarget {
    I386,
    X86_64,
}

impl ElfTarget {
    const fn class(self) -> u8 {
        match self {
            Self::I386 => 1,
            Self::X86_64 => 2,
        }
    }

    const fn machine(self) -> u16 {
        match self {
            Self::I386 => 3,
            Self::X86_64 => 62,
        }
    }

    /// glibc `FLAG_ELF_LIBC6` plus the architecture-required bits.
    const fn ld_cache_flags(self) -> i32 {
        match self {
            Self::I386 => 0x0003,
            Self::X86_64 => 0x0303,
        }
    }

    const fn fallback_directories(self) -> &'static [&'static str] {
        match self {
            Self::I386 => &[
                "/usr/lib32",
                "/lib32",
                "/usr/lib/i386-linux-gnu",
                "/lib/i386-linux-gnu",
                "/usr/lib",
                "/lib",
            ],
            Self::X86_64 => &[
                "/usr/lib64",
                "/lib64",
                "/usr/lib/x86_64-linux-gnu",
                "/lib/x86_64-linux-gnu",
                "/usr/lib",
            ],
        }
    }
}

/// Read only the ELF identification and machine fields.
fn elf_matches(path: &Path, target: ElfTarget) -> bool {
    let Ok(file) = File::open(path) else {
        return false;
    };
    if !file.metadata().is_ok_and(|metadata| metadata.is_file()) {
        return false;
    }
    let mut header = [0_u8; 20];
    if file.take(20).read_exact(&mut header).is_err() {
        return false;
    }
    elf_target(&header) == Some(target)
}

/// A little-endian 64-bit AArch64 ELF (`EM_AARCH64`, 183).
fn is_aarch64_elf(header: &[u8; 20]) -> bool {
    header[..4] == *b"\x7fELF"
        && header[4] == 2
        && header[5] == 1
        && u16::from_le_bytes([header[18], header[19]]) == 183
}

/// Classify a little-endian ELF identification and machine header.
fn elf_target(header: &[u8; 20]) -> Option<ElfTarget> {
    if header[..4] != *b"\x7fELF" || header[5] != 1 {
        return None;
    }
    let machine = u16::from_le_bytes([header[18], header[19]]);
    [ElfTarget::I386, ElfTarget::X86_64]
        .into_iter()
        .find(|target| header[4] == target.class() && machine == target.machine())
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct LdCacheEntry {
    flags: i32,
    soname: String,
    path: PathBuf,
}

const LD_CACHE_OLD_MAGIC: &[u8] = b"ld.so-1.7.0";
const LD_CACHE_NEW_MAGIC: &[u8] = b"glibc-ld.so.cache1.1";
const LD_CACHE_OLD_HEADER: usize = 16;
const LD_CACHE_OLD_ENTRY: usize = 12;
const LD_CACHE_NEW_HEADER: usize = 48;
const LD_CACHE_NEW_ENTRY: usize = 24;

/// Parse glibc's `/etc/ld.so.cache` in the standalone new format, the old
/// libc5-compatible format, or the old format followed by the new one.
/// Malformed input yields `None`; no offset is trusted without bounds checks.
fn parse_ld_so_cache(bytes: &[u8]) -> Option<Vec<LdCacheEntry>> {
    if bytes.starts_with(LD_CACHE_NEW_MAGIC) {
        return parse_ld_so_cache_new(bytes);
    }
    if !bytes.starts_with(LD_CACHE_OLD_MAGIC) {
        return None;
    }
    let count = usize::try_from(read_u32(bytes, 12)?).ok()?;
    if count > LD_SO_CACHE_ENTRY_LIMIT {
        return None;
    }
    let table_end = LD_CACHE_OLD_HEADER.checked_add(count.checked_mul(LD_CACHE_OLD_ENTRY)?)?;
    if table_end > bytes.len() {
        return None;
    }
    // A combined cache places the new format at the next 8-byte boundary;
    // prefer it because it carries the same entries with 64-bit hwcaps.
    let new_offset = table_end.checked_add(7)? & !7;
    if let Some(new) = bytes.get(new_offset..)
        && new.starts_with(LD_CACHE_NEW_MAGIC)
    {
        return parse_ld_so_cache_new(new);
    }
    // Old-format string offsets are relative to the string table after the
    // entry array.
    let strings = &bytes[table_end..];
    (0..count)
        .map(|index| {
            let entry = LD_CACHE_OLD_HEADER + index * LD_CACHE_OLD_ENTRY;
            Some(LdCacheEntry {
                flags: read_u32(bytes, entry)? as i32,
                soname: read_c_string(strings, read_u32(bytes, entry + 4)?)?,
                path: PathBuf::from(read_c_string(strings, read_u32(bytes, entry + 8)?)?),
            })
        })
        .collect()
}

/// New-format string offsets are relative to the start of the new header.
fn parse_ld_so_cache_new(bytes: &[u8]) -> Option<Vec<LdCacheEntry>> {
    let count = usize::try_from(read_u32(bytes, 20)?).ok()?;
    if count > LD_SO_CACHE_ENTRY_LIMIT {
        return None;
    }
    let table_end = LD_CACHE_NEW_HEADER.checked_add(count.checked_mul(LD_CACHE_NEW_ENTRY)?)?;
    if table_end > bytes.len() {
        return None;
    }
    (0..count)
        .map(|index| {
            let entry = LD_CACHE_NEW_HEADER + index * LD_CACHE_NEW_ENTRY;
            Some(LdCacheEntry {
                flags: read_u32(bytes, entry)? as i32,
                soname: read_c_string(bytes, read_u32(bytes, entry + 4)?)?,
                path: PathBuf::from(read_c_string(bytes, read_u32(bytes, entry + 8)?)?),
            })
        })
        .collect()
}

fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    let field = bytes.get(offset..offset.checked_add(4)?)?;
    Some(u32::from_le_bytes(field.try_into().ok()?))
}

fn read_c_string(bytes: &[u8], offset: u32) -> Option<String> {
    let tail = bytes.get(usize::try_from(offset).ok()?..)?;
    let length = tail.iter().take(4096).position(|byte| *byte == 0)?;
    String::from_utf8(tail[..length].to_vec()).ok()
}

/// Locate a shared library of `target` class through the loader cache, then
/// through `fallback_directories` (in production the target's fixed
/// distribution directories). Every candidate's ELF header must match, so a
/// stale cache or a same-named library of the other class does not count.
fn library_present(
    cache: &[LdCacheEntry],
    soname: &str,
    target: ElfTarget,
    fallback_directories: &[impl AsRef<Path>],
) -> bool {
    cache
        .iter()
        .filter(|entry| entry.soname == soname && entry.flags == target.ld_cache_flags())
        .any(|entry| elf_matches(&entry.path, target))
        || fallback_directories
            .iter()
            .any(|directory| elf_matches(&directory.as_ref().join(soname), target))
}

/// 32-bit libraries a direct (non-WoW64) x86-64 Wine profile needs. The
/// x86-64 row runs Wine in WoW64 mode, which needs no host multilib, so
/// these are gathered as diagnostic evidence only and no Host
/// capability requires them. Each group is satisfied by any one of its sonames.
const I386_LIBRARY_REQUIREMENTS: [&[&str]; 7] = [
    &["libc.so.6"],
    &["libX11.so.6"],
    &["libGL.so.1"],
    &["libfreetype.so.6"],
    &["libfontconfig.so.1"],
    &["libgnutls.so.30"],
    &["libasound.so.2", "libpulse.so.0"],
];

fn missing_i386_libraries(
    cache: &[LdCacheEntry],
    fallback_directories: &[impl AsRef<Path>],
) -> Vec<String> {
    I386_LIBRARY_REQUIREMENTS
        .iter()
        .filter(|group| {
            !group
                .iter()
                .any(|soname| library_present(cache, soname, ElfTarget::I386, fallback_directories))
        })
        .map(|group| group.join(" or "))
        .collect()
}

/// The 64-bit libraries the WoW64 runtime and the game load that are not
/// installed. Wine `dlopen`s them, so a missing one never stops Wine from
/// starting; the prefix or the game fails later instead.
fn missing_wine_libraries(
    cache: &[LdCacheEntry],
    fallback_directories: &[impl AsRef<Path>],
) -> Vec<WineLibrary> {
    WineLibrary::ALL
        .into_iter()
        .filter(|library| {
            !library.sonames().iter().any(|soname| {
                library_present(cache, soname, ElfTarget::X86_64, fallback_directories)
            })
        })
        .collect()
}

fn native_guest_evidence() -> GuestExecutionEvidence {
    let cache = read_bounded_bytes(Path::new("/etc/ld.so.cache"), LD_SO_CACHE_LIMIT)
        .ok()
        .and_then(|bytes| parse_ld_so_cache(&bytes))
        .unwrap_or_default();
    GuestExecutionEvidence::Native {
        ia32_emulation: current_ia32_emulation(),
        i386_loader_present: elf_matches(Path::new("/lib/ld-linux.so.2"), ElfTarget::I386),
        missing_i386_libraries: missing_i386_libraries(
            &cache,
            ElfTarget::I386.fallback_directories(),
        ),
        missing_wine_libraries: missing_wine_libraries(
            &cache,
            ElfTarget::X86_64.fallback_directories(),
        ),
        // Diagnostic only: Wine-GE's x86-64 ntdll.so links libunwind, but the
        // WoW64 row does not require it.
        libunwind_present: library_present(
            &cache,
            "libunwind.so.8",
            ElfTarget::X86_64,
            ElfTarget::X86_64.fallback_directories(),
        ),
        x11_display_available: x11_display_available(
            env::var_os("DISPLAY").as_deref(),
            Path::new("/tmp/.X11-unix"),
        ),
    }
}

/// Whether the translator runs an x86-64 program from its RootFS, in the same
/// cleared environment every guest request gets.
fn translator_rootfs_ready(guest: &GuestExecutor) -> bool {
    if !guest
        .interpreter()
        .is_some_and(|interpreter| is_usable_executable(&interpreter))
    {
        return false;
    }
    let Some(home) = validated_directory_environment("HOME") else {
        return false;
    };
    let Ok(invocation) = guest.shape(Path::new(TRANSLATOR_PROBE_PROGRAM), &[]) else {
        return false;
    };
    let mut command = Command::new(&invocation.program);
    command
        .args(&invocation.arguments)
        .env_clear()
        .env("HOME", home)
        .env("LANG", "C.UTF-8")
        .env("PATH", "/usr/bin:/usr/sbin")
        .envs(invocation.environment);
    if let Some(runtime_directory) = validated_directory_environment("XDG_RUNTIME_DIR") {
        command.env("XDG_RUNTIME_DIR", runtime_directory);
    }
    run_bounded(command, TRANSLATOR_PROBE_TIMEOUT, HOST_COMMAND_OUTPUT_LIMIT)
        .is_ok_and(|output| output.status.success())
}

fn query_fex_package() -> io::Result<Option<String>> {
    let rpm = Path::new("/usr/bin/rpm");
    if !is_usable_executable(rpm) {
        // Package metadata is diagnostic context, not a Host capability. A
        // Rocknix-family image may provide a working FEX executable without
        // RPM, so absence of this optional query must not invalidate the
        // executable capability probe.
        return Ok(None);
    }
    let mut command = Command::new(rpm);
    command
        .args([
            "-q",
            "--qf",
            "%{NAME}-%{VERSION}-%{RELEASE}.%{ARCH}",
            "fex-emu",
        ])
        .env_clear()
        .env("LC_ALL", "C");
    let output = run_bounded(command, HOST_COMMAND_TIMEOUT, HOST_COMMAND_OUTPUT_LIMIT)?;
    classify_fex_package_query(output)
}

fn classify_fex_package_query(output: BoundedOutput) -> io::Result<Option<String>> {
    if !output.status.success() {
        return match output.status.code() {
            Some(1) => Ok(None),
            _ => Err(io::Error::other("RPM package query failed")),
        };
    }
    String::from_utf8(output.stdout)
        .map(Some)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "RPM output is not UTF-8"))
}

fn is_executable(path: &Path) -> bool {
    fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

fn runtime_execution_preflight(
    runtime: &RuntimePreflightRequest,
    guest: &GuestExecutor,
) -> RuntimeExecutionEvidence {
    let executor_ready = match guest {
        GuestExecutor::Translated { interpreter, .. } => is_usable_executable(interpreter),
        GuestExecutor::Direct => true,
        GuestExecutor::Unavailable(_) => false,
        GuestExecutor::Chosen(_) => guest
            .interpreter()
            .is_some_and(|interpreter| is_usable_executable(&interpreter)),
    };
    if !runtime.execution_allowed()
        || !executor_ready
        || !is_executable(runtime.wine())
        || !is_executable(runtime.wine_preloader())
        || !runtime.prefix().is_dir()
    {
        return RuntimeExecutionEvidence::Unavailable;
    }
    run_runtime_execution_preflight(runtime, guest)
}

/// `wine --version` through `guest`, once the runtime and prefix are there.
fn run_runtime_execution_preflight(
    runtime: &RuntimePreflightRequest,
    guest: &GuestExecutor,
) -> RuntimeExecutionEvidence {
    let Some(home) = validated_directory_environment("HOME") else {
        return RuntimeExecutionEvidence::Unavailable;
    };
    let Some(paths) = runtime.command_paths() else {
        return RuntimeExecutionEvidence::Unavailable;
    };
    let mut arguments = Vec::with_capacity(2);
    if runtime.loader_takes_wine_path() {
        arguments.push(paths.wine.into_os_string());
    }
    arguments.push(OsString::from("--version"));
    let Ok(invocation) = guest.shape(&paths.wine_preloader, &arguments) else {
        return RuntimeExecutionEvidence::Unavailable;
    };
    let mut command = Command::new(&invocation.program);
    command.args(&invocation.arguments);
    if !matches!(runtime.configure_command(&mut command), Ok(true)) {
        return RuntimeExecutionEvidence::Unavailable;
    }
    command
        .env_clear()
        .env("HOME", home)
        .env("LANG", "C.UTF-8")
        .env("PATH", "/usr/bin:/usr/sbin")
        .env("WINEPREFIX", paths.prefix)
        .env("WINEDEBUG", "-all")
        .envs(invocation.environment);
    if let Some(runtime_directory) = validated_directory_environment("XDG_RUNTIME_DIR") {
        command.env("XDG_RUNTIME_DIR", runtime_directory);
    }

    let direct = matches!(guest, GuestExecutor::Direct);
    if direct {
        let refusal = runtime
            .guest_image()
            .map_or(Some(DirectGuestRefusal::UnrecognizedImage), |image| {
                direct_guest_refusal(image, current_ia32_emulation())
            });
        match refusal {
            Some(DirectGuestRefusal::Ia32ExecutionDisabled) => {
                return RuntimeExecutionEvidence::Blocked(
                    GuestExecutionBlocker::Ia32ExecutionDisabled,
                );
            }
            Some(DirectGuestRefusal::UnrecognizedImage) => {
                return RuntimeExecutionEvidence::Unavailable;
            }
            None => {}
        }
    }
    match run_bounded(command, HOST_COMMAND_TIMEOUT, HOST_COMMAND_OUTPUT_LIMIT) {
        Ok(output) => {
            classify_runtime_preflight_for(direct, &output, runtime.expected_version(), || {
                current_ia32_emulation() == Ia32Emulation::Disabled
            })
        }
        Err(error) if direct => classify_direct_spawn_error(&error),
        Err(_) => RuntimeExecutionEvidence::Unavailable,
    }
}

/// Direct execution surfaces host refusal as an exec errno. Translation keeps
/// its established evidence: every spawn failure is Unavailable.
fn classify_direct_spawn_error(error: &io::Error) -> RuntimeExecutionEvidence {
    match error.raw_os_error() {
        Some(code) if code == rustix::io::Errno::NOEXEC.raw_os_error() => {
            RuntimeExecutionEvidence::Blocked(GuestExecutionBlocker::Ia32ExecutionDisabled)
        }
        Some(code) if code == rustix::io::Errno::ACCESS.raw_os_error() => {
            RuntimeExecutionEvidence::Blocked(GuestExecutionBlocker::ExecutionDenied)
        }
        _ => RuntimeExecutionEvidence::Unavailable,
    }
}

#[cfg(test)]
fn classify_runtime_preflight(
    direct: bool,
    output: &BoundedOutput,
    ia32_disabled_by_kernel: impl FnOnce() -> bool,
) -> RuntimeExecutionEvidence {
    classify_runtime_preflight_for(
        direct,
        output,
        Some(crate::runtime::current().version),
        ia32_disabled_by_kernel,
    )
}

/// `expected_version` is the pinned runtime's exact `wine --version`; `None`
/// (a custom-source runtime) accepts any Wine version string.
fn classify_runtime_preflight_for(
    direct: bool,
    output: &BoundedOutput,
    expected_version: Option<&str>,
    ia32_disabled_by_kernel: impl FnOnce() -> bool,
) -> RuntimeExecutionEvidence {
    if output.status.success() {
        let reported = String::from_utf8_lossy(&output.stdout);
        let reported = reported.trim();
        let accepted = match expected_version {
            Some(expected) => reported == expected,
            None => {
                reported.starts_with("wine-")
                    && reported.len() <= 128
                    && !reported.chars().any(char::is_control)
            }
        };
        return if accepted {
            RuntimeExecutionEvidence::Passed
        } else {
            RuntimeExecutionEvidence::Failed
        };
    }
    if !direct {
        return RuntimeExecutionEvidence::Failed;
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    if stderr.contains("error while loading shared libraries")
        || (stderr.contains("ld-linux.so.2") && stderr.contains("could not open"))
    {
        return RuntimeExecutionEvidence::Blocked(GuestExecutionBlocker::SharedLibrariesMissing);
    }
    // glibc's execvp retries an ENOEXEC image through /bin/sh, so a disabled
    // IA-32 ABI can also surface as the shell's "cannot execute" exit 126.
    if stderr.contains("Exec format error")
        || stderr.contains("cannot execute binary file")
        || output.status.code() == Some(126)
        || ia32_disabled_by_kernel()
    {
        return RuntimeExecutionEvidence::Blocked(GuestExecutionBlocker::Ia32ExecutionDisabled);
    }
    // A shell diagnostic without Wine's version output means the shell, not
    // Wine, ran: host-side evidence that says nothing about the runtime.
    if !String::from_utf8_lossy(&output.stdout)
        .trim_start()
        .starts_with("wine-")
        && shell_diagnostic(&stderr)
    {
        return RuntimeExecutionEvidence::Unavailable;
    }
    RuntimeExecutionEvidence::Failed
}

/// `sh: ...`, dash's `<script>: <line>: <message>`, or a shell syntax error.
fn shell_diagnostic(stderr: &str) -> bool {
    stderr.lines().any(|line| {
        let mut fields = line.splitn(3, ": ");
        let first = fields.next().unwrap_or_default();
        let line_number = fields.next().is_some_and(|field| {
            !field.is_empty() && field.bytes().all(|byte| byte.is_ascii_digit())
        });
        first == "sh"
            || first.ends_with("/sh")
            || (line_number && fields.next().is_some())
            || line.contains("Syntax error")
            || line.contains("syntax error")
    })
}

#[derive(Debug)]
struct BoundedOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

/// Run a short host command with a deadline. Stdout beyond the limit is an
/// error; stderr is diagnostic, so it is drained and truncated at the limit.
/// Once the child has exited and stdout has closed, stderr is drained only for
/// [`PROCESS_DRAIN_GRACE`]: a daemon the child started (such as FEXServer)
/// may keep the inherited stderr open indefinitely.
fn run_bounded(
    mut command: Command,
    timeout: Duration,
    output_limit: u64,
) -> io::Result<BoundedOutput> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let (Some(mut stdout), Some(mut stderr)) = (child.stdout.take(), child.stderr.take()) else {
        terminate_child(&mut child);
        return Err(io::Error::other("child output was not piped"));
    };
    for descriptor in [stdout.as_fd(), stderr.as_fd()] {
        let result = rustix::fs::fcntl_getfl(descriptor).and_then(|flags| {
            rustix::fs::fcntl_setfl(descriptor, flags | rustix::fs::OFlags::NONBLOCK)
        });
        if let Err(error) = result {
            terminate_child(&mut child);
            return Err(io::Error::from(error));
        }
    }
    let limit = usize::try_from(output_limit).unwrap_or(usize::MAX);
    let deadline = Instant::now() + timeout;
    let mut status = None;
    let mut stdout_bytes = Vec::new();
    let mut stderr_bytes = Vec::new();
    let mut stdout_closed = false;
    let mut stderr_closed = false;
    let mut stderr_grace_deadline = None;
    loop {
        let drained = drain_nonblocking(&mut stdout, &mut stdout_closed, |bytes| {
            stdout_bytes.extend_from_slice(bytes);
            stdout_bytes.len() <= limit
        })
        .and_then(|()| {
            drain_nonblocking(&mut stderr, &mut stderr_closed, |bytes| {
                let accepted = limit.saturating_sub(stderr_bytes.len()).min(bytes.len());
                stderr_bytes.extend_from_slice(&bytes[..accepted]);
                true
            })
        });
        if let Err(error) = drained {
            if status.is_none() {
                terminate_child(&mut child);
            }
            return Err(error);
        }
        if status.is_none() {
            status = match child.try_wait() {
                Ok(status) => status,
                Err(error) => {
                    terminate_child(&mut child);
                    return Err(error);
                }
            };
        }
        if let Some(status) = status
            && stdout_closed
            && (stderr_closed
                || Instant::now()
                    >= *stderr_grace_deadline
                        .get_or_insert_with(|| Instant::now() + PROCESS_DRAIN_GRACE))
        {
            return Ok(BoundedOutput {
                status,
                stdout: stdout_bytes,
                stderr: stderr_bytes,
            });
        }
        if Instant::now() >= deadline {
            if status.is_none() {
                terminate_child(&mut child);
            }
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "host command timed out",
            ));
        }
        thread::sleep(Duration::from_millis(20));
    }
}

/// Read everything currently available. `accept` returns `false` when the
/// stream exceeded its bound.
fn drain_nonblocking(
    stream: &mut impl Read,
    closed: &mut bool,
    mut accept: impl FnMut(&[u8]) -> bool,
) -> io::Result<()> {
    let mut buffer = [0_u8; 1024];
    while !*closed {
        match stream.read(&mut buffer) {
            Ok(0) => *closed = true,
            Ok(count) => {
                if !accept(&buffer[..count]) {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "host command output exceeds limit",
                    ));
                }
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn terminate_child(child: &mut std::process::Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn validated_directory_environment(name: &str) -> Option<OsString> {
    let value = env::var_os(name)?;
    let path = Path::new(&value);
    (path.is_absolute() && fs::metadata(path).is_ok_and(|metadata| metadata.is_dir()))
        .then_some(value)
}

fn vulkan_loader_present() -> bool {
    [
        "/usr/lib64/libvulkan.so.1",
        "/lib64/libvulkan.so.1",
        "/usr/lib/aarch64-linux-gnu/libvulkan.so.1",
        "/usr/lib/x86_64-linux-gnu/libvulkan.so.1",
        "/lib/x86_64-linux-gnu/libvulkan.so.1",
        "/usr/lib/libvulkan.so.1",
    ]
    .into_iter()
    .any(|path| fs::metadata(path).is_ok_and(|metadata| metadata.is_file()))
}

/// Diagnostic ICD discovery. GPUI's executed Vulkan evidence remains the
/// authoritative hardware-graphics capability.
fn vulkan_driver_present() -> bool {
    vulkan_icd_search_paths(|name| env::var_os(name))
        .iter()
        .any(|path| vulkan_icd_path_present(path))
}

/// Follow the Vulkan loader's driver search: an explicit
/// `VK_DRIVER_FILES`/`VK_ICD_FILENAMES` list replaces the standard XDG and
/// system directories.
fn vulkan_icd_search_paths(environment: impl Fn(&str) -> Option<OsString>) -> Vec<PathBuf> {
    let absolute_list = |value: OsString| {
        env::split_paths(&value)
            .filter(|path| path.is_absolute())
            .take(VULKAN_ICD_SEARCH_LIMIT)
            .collect::<Vec<_>>()
    };
    if let Some(explicit) = environment("VK_DRIVER_FILES")
        .or_else(|| environment("VK_ICD_FILENAMES"))
        .filter(|value| !value.is_empty())
    {
        return absolute_list(explicit);
    }

    let home = environment("HOME").map(PathBuf::from);
    let config_home = environment("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| home.as_ref().map(|home| home.join(".config")));
    let data_home = environment("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| home.as_ref().map(|home| home.join(".local/share")));
    let config_directories = environment("XDG_CONFIG_DIRS")
        .filter(|value| !value.is_empty())
        .map_or_else(|| vec![PathBuf::from("/etc/xdg")], absolute_list);
    let data_directories = environment("XDG_DATA_DIRS")
        .filter(|value| !value.is_empty())
        .map_or_else(
            || {
                vec![
                    PathBuf::from("/usr/local/share"),
                    PathBuf::from("/usr/share"),
                ]
            },
            absolute_list,
        );

    config_home
        .into_iter()
        .chain(config_directories)
        .chain([PathBuf::from("/etc")])
        .chain(data_home)
        .chain(data_directories)
        // The standard system directories stay searched even when a session
        // narrows XDG_DATA_DIRS, preserving the established discovery set.
        .chain([
            PathBuf::from("/usr/local/share"),
            PathBuf::from("/usr/share"),
            PathBuf::from("/run/opengl-driver/share"),
        ])
        .filter(|base| base.is_absolute())
        .map(|base| base.join("vulkan/icd.d"))
        .take(VULKAN_ICD_SEARCH_LIMIT)
        .collect()
}

fn vulkan_icd_path_present(path: &Path) -> bool {
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_dir() => vulkan_icd_directory_present(path),
        Ok(metadata) if metadata.is_file() => vulkan_icd_file_valid(path),
        _ => false,
    }
}

fn vulkan_icd_directory_present(directory: &Path) -> bool {
    let Ok(entries) = fs::read_dir(directory) else {
        return false;
    };
    for (index, entry) in entries.enumerate() {
        if index >= 128 {
            return false;
        }
        let Ok(entry) = entry else {
            return false;
        };
        if !entry.file_type().is_ok_and(|kind| kind.is_file())
            || entry.path().extension() != Some(OsStr::new("json"))
        {
            continue;
        }
        if vulkan_icd_file_valid(&entry.path()) {
            return true;
        }
    }
    false
}

fn vulkan_icd_file_valid(path: &Path) -> bool {
    read_bounded(path, HOST_FILE_LIMIT)
        .ok()
        .and_then(|contents| parse_icd_library_path(&contents))
        .is_some_and(|library| !library.trim().is_empty())
}

fn parse_icd_library_path(contents: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(contents)
        .ok()?
        .get("ICD")?
        .get("library_path")?
        .as_str()
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use std::os::unix::process::ExitStatusExt;

    use super::*;

    #[test]
    fn the_capture_leaves_out_wine_debug_noise_and_keeps_the_error() {
        let output = "0020:0024:trace:loaddll:build_module Loaded L\"ntdll.dll\" at 6FFFFF5A0000: builtin\n\
                      0020:0024:trace:winsock:select read 0x22f100, timeout 0x0\r\n\
                      0020:0024:trace:winsock:connect socket 0x1a4, len 16\r\n\
                      err:module:import_dll Library X.dll not found\n\
                      partial";
        let capture = Arc::new(Mutex::new(BoundedCapture::new(4096)));
        let completion = Arc::new(DrainCompletion::default());
        // The other stream of a program, done.
        completion.complete_stream();
        drain_stream(
            io::Cursor::new(output.as_bytes().to_vec()),
            Arc::clone(&capture),
            Arc::clone(&completion),
            false,
        );
        completion.wait_for_streams(Duration::from_secs(10));
        let captured = String::from_utf8(capture.lock().unwrap().contents()).unwrap();
        assert_eq!(
            captured,
            "0020:0024:trace:winsock:connect socket 0x1a4, len 16\r\n\
             err:module:import_dll Library X.dll not found\npartial"
        );
    }

    #[test]
    fn a_long_line_goes_into_the_capture_whole() {
        let line = format!("{}\n", "x".repeat(CAPTURE_LINE * 2 + 5));
        let capture = Arc::new(Mutex::new(BoundedCapture::new(CAPTURE_LINE * 4)));
        let completion = Arc::new(DrainCompletion::default());
        completion.complete_stream();
        drain_stream(
            io::Cursor::new(line.clone().into_bytes()),
            Arc::clone(&capture),
            Arc::clone(&completion),
            false,
        );
        completion.wait_for_streams(Duration::from_secs(10));
        assert_eq!(capture.lock().unwrap().contents(), line.into_bytes());
    }

    #[test]
    fn a_long_output_keeps_its_start_and_its_end() {
        let mut capture = BoundedCapture::new(16);
        capture.append(b"0123456789");
        assert_eq!(capture.contents(), b"0123456789");
        assert!(!capture.truncated);
        capture.append(b"abcdef");
        // All of it while it fits.
        assert_eq!(capture.contents(), b"0123456789abcdef");
        capture.append(b"ERROR!");
        assert!(capture.truncated);
        assert_eq!(
            String::from_utf8(capture.contents()).unwrap(),
            "01234567\n[6 bytes of output left out]\nefERROR!"
        );
    }

    /// `output` through [`drain_stream`] into a capture of `limit` bytes.
    fn drained(output: String, limit: usize) -> String {
        let capture = Arc::new(Mutex::new(BoundedCapture::new(limit)));
        let completion = Arc::new(DrainCompletion::default());
        completion.complete_stream();
        drain_stream(
            io::Cursor::new(output.into_bytes()),
            Arc::clone(&capture),
            Arc::clone(&completion),
            false,
        );
        completion.wait_for_streams(Duration::from_secs(10));
        String::from_utf8(capture.lock().unwrap().contents()).unwrap()
    }

    #[test]
    fn the_capture_takes_network_lines_twenty_a_second() {
        let output = (0..100)
            .map(|index| format!("0020:0024:trace:winsock:connect socket {index}\n"))
            .collect::<String>();
        // The 100 come in one second, or in two when the second changes
        // while they are read.
        let network = drained(output, 64 * 1024)
            .matches(":winsock:connect")
            .count();
        assert!((20..=40).contains(&network), "{network}");
    }

    #[test]
    fn a_long_runs_capture_ends_with_its_error() {
        let mut output = "starting\n".to_owned();
        for _ in 0..200 {
            output.push_str("fixme:ole:noise\n");
        }
        output.push_str("err:module:import_dll Library X.dll not found\n");
        let captured = drained(output, 1024);
        assert!(captured.starts_with("starting\n"), "{captured}");
        assert!(captured.contains("bytes of output left out"), "{captured}");
        assert!(
            captured.ends_with("err:module:import_dll Library X.dll not found\n"),
            "{captured}"
        );
    }

    #[test]
    fn bounded_host_output_rejects_overflow() {
        let mut command = Command::new("/usr/bin/printf");
        command.arg("12345").env_clear();

        let error = run_bounded(command, Duration::from_secs(1), 4).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn bounded_host_command_enforces_deadline() {
        let mut command = Command::new("/usr/bin/sleep");
        command.arg("1").env_clear();

        let error = run_bounded(command, Duration::from_millis(20), 4).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }

    #[test]
    fn bounded_host_reads_accept_only_regular_files_without_following_links() {
        let directory = tempfile::tempdir().unwrap();
        let regular = directory.path().join("regular.json");
        fs::write(
            &regular,
            r#"{"ICD":{"library_path":"libvulkan_vendor.so"}}"#,
        )
        .unwrap();
        let link = directory.path().join("link.json");
        std::os::unix::fs::symlink(&regular, &link).unwrap();
        let fifo = directory.path().join("fifo.json");
        rustix::fs::mknodat(
            rustix::fs::CWD,
            &fifo,
            rustix::fs::FileType::Fifo,
            rustix::fs::Mode::from_raw_mode(0o600),
            0,
        )
        .unwrap();

        assert!(read_bounded(&regular, HOST_FILE_LIMIT).is_ok());
        assert_eq!(
            read_bounded(&link, HOST_FILE_LIMIT)
                .unwrap_err()
                .raw_os_error(),
            Some(rustix::io::Errno::LOOP.raw_os_error())
        );
        // A FIFO with no writer would block a plain open; this returns.
        assert_eq!(
            read_bounded(&fifo, HOST_FILE_LIMIT).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        assert!(read_bounded(directory.path(), HOST_FILE_LIMIT).is_err());
        assert!(vulkan_icd_path_present(&regular));
        assert!(!vulkan_icd_path_present(&link));
        assert!(!vulkan_icd_path_present(&fifo));
    }

    #[test]
    fn icd_path_must_be_the_parsed_library_field() {
        assert_eq!(
            parse_icd_library_path(
                r#"{"ICD":{"library_path":"/usr/lib64/libvulkan_freedreno.so"}}"#,
            )
            .as_deref(),
            Some("/usr/lib64/libvulkan_freedreno.so")
        );
        assert_eq!(
            parse_icd_library_path(r#"{"note":"/usr/lib64/libvulkan_freedreno.so","ICD":{}}"#,),
            None
        );
    }

    #[test]
    fn vulkan_driver_discovery_accepts_any_valid_standard_icd_shape() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(
            directory.path().join("vendor_icd.aarch64.json"),
            r#"{"ICD":{"library_path":"libvulkan_vendor.so"}}"#,
        )
        .unwrap();
        assert!(vulkan_icd_directory_present(directory.path()));

        fs::write(directory.path().join("vendor_icd.aarch64.json"), b"{}").unwrap();
        assert!(!vulkan_icd_directory_present(directory.path()));
    }

    #[test]
    fn rpm_absence_is_distinct_from_probe_failure() {
        assert_eq!(
            classify_fex_package_query(BoundedOutput {
                status: ExitStatus::from_raw(1 << 8),
                stdout: Vec::new(),
                stderr: Vec::new(),
            })
            .unwrap(),
            None
        );
        assert!(
            classify_fex_package_query(BoundedOutput {
                status: ExitStatus::from_raw(2 << 8),
                stdout: Vec::new(),
                stderr: Vec::new(),
            })
            .is_err()
        );
        assert!(
            classify_fex_package_query(BoundedOutput {
                status: ExitStatus::from_raw(0),
                stdout: vec![0xff],
                stderr: Vec::new(),
            })
            .is_err()
        );
    }

    #[test]
    fn bounded_host_stderr_is_truncated_rather_than_rejected() {
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", "printf 0123456789 >&2; printf ok"])
            .env_clear();

        let output = run_bounded(command, Duration::from_secs(2), 4).unwrap();

        assert_eq!(output.stdout, b"ok");
        assert_eq!(output.stderr, b"0123");
    }

    #[test]
    fn bounded_host_command_does_not_wait_for_a_daemon_holding_stderr() {
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", "(sleep 3 >/dev/null &) ; echo out"])
            .env_clear();
        let started = Instant::now();

        let output = run_bounded(command, Duration::from_secs(5), 64).unwrap();

        assert!(output.status.success());
        assert_eq!(output.stdout, b"out\n");
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "stderr drain waited {:?}",
            started.elapsed()
        );
    }

    fn exit(code: i32) -> ExitStatus {
        ExitStatus::from_raw(code << 8)
    }

    fn output(code: i32, stdout: &[u8], stderr: &[u8]) -> BoundedOutput {
        BoundedOutput {
            status: exit(code),
            stdout: stdout.to_vec(),
            stderr: stderr.to_vec(),
        }
    }

    #[test]
    fn translated_executor_prepends_the_interpreter_and_only_set_passthrough_names() {
        let executor = GuestExecutor::select(
            &HostPlatform::Aarch64,
            || Some(PathBuf::from("/usr/bin/FEXInterpreter")),
            |name| (name == "FEX_ROOTFS").then(|| OsString::from("/rootfs")),
        );

        let invocation = executor
            .shape(
                Path::new("/proc/self/fd/212"),
                &[
                    OsString::from("/proc/self/fd/216/wine"),
                    OsString::from("--version"),
                ],
            )
            .unwrap();

        assert_eq!(invocation.program, Path::new("/usr/bin/FEXInterpreter"));
        assert_eq!(
            invocation.arguments,
            [
                OsString::from("/proc/self/fd/212"),
                OsString::from("/proc/self/fd/216/wine"),
                OsString::from("--version"),
            ]
        );
        assert_eq!(
            invocation.environment,
            [(OsString::from("FEX_ROOTFS"), OsString::from("/rootfs"))]
        );
        assert!(!format!("{executor:?}").contains("/rootfs"));

        let without_environment = GuestExecutor::select(
            &HostPlatform::Aarch64,
            || Some(PathBuf::from("/usr/bin/FEX")),
            |_| None,
        );
        assert!(
            without_environment
                .shape(Path::new("/proc/self/fd/212"), &[])
                .unwrap()
                .environment
                .is_empty()
        );
        assert!(
            GuestExecutor::translated(
                "/usr/bin/FEX",
                [(OsString::from("LD_PRELOAD"), OsString::from("/evil.so"))],
            )
            .shape(Path::new("/proc/self/fd/212"), &[])
            .unwrap()
            .environment
            .is_empty(),
            "only accepted translator names pass through"
        );
    }

    #[test]
    fn direct_executor_executes_the_guest_unchanged() {
        let invocation = GuestExecutor::Direct
            .shape(
                Path::new("/proc/self/fd/212"),
                &[
                    OsString::from("/proc/self/fd/216/wine"),
                    OsString::from("Game.exe"),
                ],
            )
            .unwrap();

        assert_eq!(invocation.program, Path::new("/proc/self/fd/212"));
        assert_eq!(
            invocation.arguments,
            [
                OsString::from("/proc/self/fd/216/wine"),
                OsString::from("Game.exe"),
            ]
        );
        assert!(invocation.environment.is_empty());
    }

    #[test]
    fn executor_selection_follows_the_compile_target_row() {
        assert_eq!(
            GuestExecutor::select(&HostPlatform::X86_64, || unreachable!(), |_| None),
            GuestExecutor::Direct
        );
        assert_eq!(
            GuestExecutor::select(&HostPlatform::Aarch64, || None, |_| None),
            GuestExecutor::Unavailable(GuestExecutorUnavailable::TranslatorMissing)
        );
        let unknown = GuestExecutor::select(
            &HostPlatform::Other("riscv64".to_owned()),
            || unreachable!(),
            |_| None,
        );
        assert_eq!(
            unknown,
            GuestExecutor::Unavailable(GuestExecutorUnavailable::ArchitectureUnsupported)
        );
        assert_eq!(
            unknown
                .shape(Path::new("/proc/self/fd/212"), &[])
                .unwrap_err()
                .kind(),
            io::ErrorKind::Unsupported
        );
    }

    #[test]
    fn guest_descriptor_mappings_are_identical_for_both_executors() {
        let directory = tempfile::tempdir().unwrap();
        let payload = directory.path().join("payload");
        fs::write(&payload, b"verified payload").unwrap();

        // Direct execution admits only x86 guests, so `cat` stands in for
        // one only on an x86-64 test host.
        let direct = cfg!(target_arch = "x86_64").then_some(GuestExecutor::Direct);
        for executor in direct.into_iter().chain([
            // `env` stands in for a translator: it executes its first operand.
            GuestExecutor::translated("/usr/bin/env", []),
        ]) {
            let request = ProcessRequest::guest("/usr/bin/cat")
                .with_argument("/proc/self/fd/240")
                .with_cleared_environment()
                .with_output_limit(64)
                .with_inherited_file(File::open(&payload).unwrap(), 240)
                .unwrap();
            assert!(request.is_guest());
            assert_eq!(
                request.inherited_file_descriptors().collect::<Vec<_>>(),
                [240]
            );

            let mut process = SystemHostExecution::new(executor).spawn(request).unwrap();
            let exit = process.wait().unwrap();

            assert!(exit.success());
            assert_eq!(process.output().stdout(), b"verified payload");
        }
    }

    fn refusal(error: &io::Error) -> Option<DirectGuestRefusal> {
        error
            .get_ref()
            .and_then(|inner| inner.downcast_ref::<DirectGuestRefusal>())
            .copied()
    }

    #[test]
    fn direct_guest_image_must_be_x86_elf_with_ia32_available_for_elfclass32() {
        let directory = tempfile::tempdir().unwrap();
        let fixture = |name: &str, class: u8, machine: u16| {
            let path = directory.path().join(name);
            elf_fixture(&path, class, machine);
            File::open(path).unwrap()
        };
        let i386 = fixture("i386", 1, 3);
        let amd64 = fixture("amd64", 2, 62);
        let arm64 = fixture("arm64", 2, 183);
        let mismatched = fixture("mismatched", 2, 3);
        let script = directory.path().join("script");
        fs::write(&script, b"echo not an ELF\n").unwrap();
        let script = File::open(script).unwrap();
        let directory_handle = File::open(directory.path()).unwrap();

        for ia32 in [Ia32Emulation::NotDisabled, Ia32Emulation::Unknown] {
            assert_eq!(direct_guest_refusal(&i386, ia32), None);
        }
        assert_eq!(
            direct_guest_refusal(&i386, Ia32Emulation::Disabled),
            Some(DirectGuestRefusal::Ia32ExecutionDisabled)
        );
        assert_eq!(direct_guest_refusal(&amd64, Ia32Emulation::Disabled), None);
        // An ARM64 build runs an AArch64 ELF directly (the Android app's ARM64 Wine); any other
        // build refuses it.
        assert_eq!(
            direct_guest_refusal(&arm64, Ia32Emulation::NotDisabled),
            (!cfg!(target_arch = "aarch64")).then_some(DirectGuestRefusal::UnrecognizedImage)
        );
        for image in [&mismatched, &script, &directory_handle] {
            assert_eq!(
                direct_guest_refusal(image, Ia32Emulation::NotDisabled),
                Some(DirectGuestRefusal::UnrecognizedImage)
            );
        }
    }

    #[test]
    fn direct_guest_spawn_refuses_an_image_the_shell_would_otherwise_run() {
        let directory = tempfile::tempdir().unwrap();
        let marker = directory.path().join("shell-ran");
        let script = directory.path().join("no-shebang");
        fs::write(&script, format!("touch '{}'\n", marker.display())).unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        let host = SystemHostExecution::new(GuestExecutor::Direct);

        // A path guest and the inherited-descriptor form production uses.
        for request in [
            ProcessRequest::guest(&script),
            ProcessRequest::guest("/proc/self/fd/240")
                .with_inherited_file(File::open(&script).unwrap(), 240)
                .unwrap(),
        ] {
            let error = host
                .spawn(request.with_output_limit(64))
                .err()
                .expect("a non-x86 image must not reach exec");
            assert_eq!(error.kind(), io::ErrorKind::Unsupported);
            assert_eq!(refusal(&error), Some(DirectGuestRefusal::UnrecognizedImage));
        }
        let error = host
            .spawn(ProcessRequest::guest("/proc/self/fd/240"))
            .err()
            .expect("an uninherited descriptor cannot be verified");
        assert_eq!(refusal(&error), Some(DirectGuestRefusal::UnrecognizedImage));
        assert!(!marker.exists(), "the /bin/sh ENOEXEC fallback never ran");

        // Host (non-guest) requests are not subject to the guest check.
        assert!(
            host.spawn(ProcessRequest::new("/usr/bin/true"))
                .unwrap()
                .wait()
                .unwrap()
                .success()
        );
    }

    #[test]
    fn unavailable_executor_refuses_guest_requests_but_not_host_requests() {
        let host = SystemHostExecution::new(GuestExecutor::Unavailable(
            GuestExecutorUnavailable::TranslatorMissing,
        ));

        let error = host
            .spawn(ProcessRequest::guest("/usr/bin/true"))
            .err()
            .expect("guest spawn must fail closed");
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
        assert!(
            host.spawn(ProcessRequest::new("/usr/bin/true"))
                .unwrap()
                .wait()
                .unwrap()
                .success()
        );
    }

    #[test]
    fn translator_discovery_prefers_usr_bin_fex_then_binfmt_then_fixed_candidates() {
        let directory = tempfile::tempdir().unwrap();
        let trusted = is_usable_executable;
        let binfmt = directory.path().join("binfmt_misc");
        fs::create_dir(&binfmt).unwrap();
        let preferred = directory.path().join("FEX");
        let registered = directory.path().join("FEXInterpreter");
        let fallback = directory.path().join("local-FEX");
        for path in [&preferred, &registered, &fallback] {
            fs::write(path, b"fex").unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let candidates = [preferred.to_str().unwrap(), fallback.to_str().unwrap()];
        fs::write(
            binfmt.join("FEX-x86"),
            format!(
                "enabled\ninterpreter {}\nflags: POCF\noffset 0\n",
                registered.display()
            ),
        )
        .unwrap();

        assert_eq!(
            discover_translator(&binfmt, &candidates, trusted),
            Some(preferred.clone()),
            "the certified /usr/bin/FEX wins over a binfmt FEXInterpreter"
        );

        fs::remove_file(&preferred).unwrap();
        assert_eq!(
            discover_translator(&binfmt, &candidates, trusted),
            Some(registered.clone())
        );

        fs::remove_file(binfmt.join("FEX-x86")).unwrap();
        assert_eq!(
            discover_translator(&binfmt, &candidates, trusted),
            Some(fallback.clone())
        );

        fs::write(
            binfmt.join("FEX-x86"),
            format!("interpreter {}\n", registered.display()),
        )
        .unwrap();
        fs::set_permissions(&registered, fs::Permissions::from_mode(0o644)).unwrap();
        fs::set_permissions(&fallback, fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(
            discover_translator(&binfmt, &candidates, trusted),
            None,
            "files that are not executable are skipped"
        );
        assert_eq!(discover_translator(&binfmt, &[], trusted), None);
        assert_eq!(parse_binfmt_interpreter("interpreter relative/FEX"), None);
    }

    #[test]
    fn any_executable_file_is_a_usable_translator_whoever_owns_it() {
        let directory = tempfile::tempdir().unwrap();
        let parent = directory.path().join("bin");
        fs::create_dir(&parent).unwrap();
        // A directory anyone may write to, as some handheld images have.
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o777)).unwrap();
        let program = parent.join("FEX");
        fs::write(&program, b"fex").unwrap();
        fs::set_permissions(&program, fs::Permissions::from_mode(0o777)).unwrap();
        assert!(is_usable_executable(&program));
        let link = directory.path().join("FEX-link");
        std::os::unix::fs::symlink(&program, &link).unwrap();
        assert!(is_usable_executable(&link));
        fs::set_permissions(&program, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(!is_usable_executable(&program));
        assert!(!is_usable_executable(Path::new("relative/FEX")));
        assert!(
            !is_usable_executable(&parent),
            "directories are not executables"
        );
    }

    #[test]
    fn wine_libraries_are_named_beside_its_program() {
        let fixture = tempfile::tempdir().unwrap();
        let wine = fixture.path().join("wine-ge");
        fs::create_dir_all(wine.join("bin")).unwrap();
        fs::create_dir_all(wine.join("lib/wine/i386-unix")).unwrap();
        fs::create_dir_all(wine.join("lib64/wine/x86_64-unix")).unwrap();
        let named = wine_dll_path(&wine.join("bin/wine-preloader"), Some(OsStr::new("/extra")))
            .unwrap()
            .into_string()
            .unwrap();
        assert_eq!(
            named,
            format!("{0}/lib/wine:{0}/lib64/wine:/extra", wine.display())
        );
        assert_eq!(
            wine_dll_path(&fixture.path().join("wine-preloader"), None),
            None
        );
    }

    #[test]
    fn translated_guests_get_no_host_driver_paths() {
        let request = ProcessRequest::guest("/usr/bin/printenv")
            .with_argument("VK_ICD_FILENAMES")
            .with_cleared_environment()
            .with_environment(
                "VK_ICD_FILENAMES",
                "/usr/share/vulkan/icd.d/freedreno_icd.json",
            )
            .with_environment("GLIBC_TUNABLES", "glibc.pthread.rseq=0")
            .with_output_limit(256);
        let mut process = SystemHostExecution::new(GuestExecutor::translated("/usr/bin/env", []))
            .spawn(request)
            .unwrap();
        let exit = process.wait().unwrap();
        assert!(!exit.success(), "printenv finds no VK_ICD_FILENAMES");
        assert!(process.output().stdout().is_empty());
        for name in ["VK_ICD_FILENAMES", "GLIBC_TUNABLES", "LIBGL_DRIVERS_PATH"] {
            assert!(TRANSLATED_GUEST_WITHHELD.contains(&name));
        }
    }

    #[test]
    fn the_launchers_own_descriptor_paths_name_real_files() {
        let fixture = tempfile::tempdir().unwrap();
        let helper = fixture.path().join("helper");
        fs::write(&helper, b"x").unwrap();
        let file = File::open(&helper).unwrap();
        let own = format!("/proc/{}/fd/{}", std::process::id(), file.as_raw_fd());
        assert_eq!(
            real_path(OsStr::new(&own), &[]),
            fs::canonicalize(&helper).unwrap().into_os_string()
        );
        let memfd = rustix::fs::memfd_create("helper", rustix::fs::MemfdFlags::CLOEXEC).unwrap();
        let sealed = format!("/proc/{}/fd/{}", std::process::id(), memfd.as_raw_fd());
        assert_eq!(real_path(OsStr::new(&sealed), &[]), OsString::from(&sealed));
    }

    #[test]
    fn android_host_facts_replace_os_release_and_no_fex_is_reported() {
        let facts = HostFacts {
            android_release: "13".to_owned(),
            sdk: 33,
            device_model: "Retroid Pocket Nova".to_owned(),
            gpu_name: "Adreno (TM) 740".to_owned(),
            chip_id: 0x4305_0a01,
            kernel_release: "5.15.153-android13-8".to_owned(),
            page_size: 4096,
            data_dir: "/data/data/com.pd2launcher.app".to_owned(),
        };
        let evidence = SystemHostProbe::new(None, GuestExecutor::Direct)
            .with_android_host(facts)
            .probe(&RuntimePreflightRequest::new(
                "/missing/wine".into(),
                "/missing/wine-preloader".into(),
                "/missing/prefix".into(),
            ))
            .unwrap();
        // This machine's os-release names something else.
        assert_eq!(evidence.os_id, "android");
        assert_eq!(evidence.os_version, "33");
        assert_eq!(evidence.os_variant.as_deref(), Some("Retroid Pocket Nova"));
        assert_eq!(evidence.kernel_release, "5.15.153-android13-8");
        assert_eq!(
            evidence.kernel_machine,
            rustix::system::uname().machine().to_string_lossy()
        );
        assert_eq!(evidence.fex_package, None);
        // The app's ARM64 Wine runs the guests, on any build machine, and
        // the page size is this kernel's own.
        assert_eq!(
            evidence.guest_execution,
            GuestExecutionEvidence::AppProvided {
                page_size_4k: rustix::param::page_size() == 4096
            }
        );
    }

    #[test]
    fn the_chosen_built_in_fex_runs_guests_once_installed() {
        use crate::persistence::{FexSource, LauncherPaths, LauncherSettings, SettingsStore};
        let fixture = tempfile::tempdir().unwrap();
        let paths = LauncherPaths::from_home(fixture.path());
        let root = paths.managed_root();
        fs::create_dir_all(root.join("prefix")).unwrap();
        let device = GuestExecutor::translated("/usr/bin/FEX", std::iter::empty());
        let choice = GuestExecutor::Chosen(FexChoice {
            paths: paths.clone(),
            device: Box::new(device.clone()),
        });
        // Not chosen, on an installation set up with the device's FEX.
        let resolved = choice.resolved();
        assert_eq!(resolved.executor, device);
        assert!(!resolved.pending);
        // Chosen but not installed yet: the device's FEX until Download.
        let store = SettingsStore::new(paths.settings_file());
        store
            .save(&LauncherSettings::default().with_fex_source(FexSource::Builtin))
            .unwrap();
        let resolved = choice.resolved();
        assert_eq!(resolved.executor, device);
        assert!(resolved.pending);
        // Installed: the built-in FEX, with its own RootFS, folders, and server.
        let fex = root.join("fex");
        fs::create_dir_all(fex.join("bin")).unwrap();
        fs::create_dir_all(fex.join("rootfs/Ubuntu_24_04/usr")).unwrap();
        for program in ["FEX", "FEXServer"] {
            fs::write(fex.join("bin").join(program), b"#!/bin/true\n").unwrap();
            fs::set_permissions(
                fex.join("bin").join(program),
                fs::Permissions::from_mode(0o755),
            )
            .unwrap();
        }
        let variant = crate::builtin_fex::FexVariant::current();
        fs::write(
            fex.join("installed.json"),
            serde_json::json!({
                "schema": 1,
                "fex_version": crate::builtin_fex::fex_version(),
                "variant": variant,
                "package_sha256": variant.package_sha256(),
                "rootfs_sha256": crate::builtin_fex::rootfs().sha256(),
            })
            .to_string(),
        )
        .unwrap();
        let resolved = choice.resolved();
        assert!(!resolved.pending);
        assert_eq!(choice.interpreter(), Some(fex.join("bin/FEX")));
        let invocation = choice
            .shape(
                Path::new("/game/runtime/bin/wine64"),
                &[OsString::from("wineboot")],
            )
            .unwrap();
        assert_eq!(invocation.program, fex.join("bin/FEX"));
        assert_eq!(
            invocation.arguments,
            ["/game/runtime/bin/wine64", "wineboot"].map(OsString::from)
        );
        let get = |name: &str| {
            invocation
                .environment
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| PathBuf::from(value))
        };
        assert_eq!(get("FEX_ROOTFS"), Some(fex.join("rootfs/Ubuntu_24_04")));
        assert!(get("FEX_SERVERSOCKETPATH").is_some());
        // Choosing the device's FEX again uses it at once.
        store
            .save(&LauncherSettings::default().with_fex_source(FexSource::Device))
            .unwrap();
        assert_eq!(choice.resolved().executor, device);
    }

    #[test]
    fn real_paths_replace_descriptor_paths_only_at_the_start() {
        let real = [(230, PathBuf::from("/root/.pd2launcher/game/runtime/bin"))];
        let swap = |value: &str| real_path(OsStr::new(value), &real).into_string().unwrap();
        assert_eq!(
            swap("/proc/self/fd/230/wine"),
            "/root/.pd2launcher/game/runtime/bin/wine"
        );
        assert_eq!(
            swap("/proc/self/fd/230"),
            "/root/.pd2launcher/game/runtime/bin"
        );
        assert_eq!(swap("/proc/self/fd/2300/x"), "/proc/self/fd/2300/x");
        assert_eq!(swap("wineboot"), "wineboot");
    }

    #[test]
    fn droiddecks_wrapper_takes_its_run_arguments_before_the_guest() {
        // Found by itself, or named in Settings: the same arguments.
        let found = GuestExecutor::translated(DROIDDECK_FEX, std::iter::empty());
        let set = GuestExecutor::select(
            &HostPlatform::Aarch64,
            || Some(PathBuf::from(DROIDDECK_FEX)),
            |_| None,
        );
        assert_eq!(found, set);
        let invocation = found
            .shape(Path::new("/proc/self/fd/3"), &[OsString::from("--version")])
            .unwrap();
        assert_eq!(invocation.program, Path::new(DROIDDECK_FEX));
        assert_eq!(
            invocation.arguments,
            ["run", "--mode", "on", "--", "/proc/self/fd/3", "--version"].map(OsString::from)
        );
    }

    fn new_format_cache(entries: &[(i32, &str, &str)]) -> Vec<u8> {
        let table_end = LD_CACHE_NEW_HEADER + entries.len() * LD_CACHE_NEW_ENTRY;
        let mut strings = Vec::new();
        let mut table = Vec::new();
        for (flags, soname, path) in entries {
            let key = (table_end + strings.len()) as u32;
            strings.extend_from_slice(soname.as_bytes());
            strings.push(0);
            let value = (table_end + strings.len()) as u32;
            strings.extend_from_slice(path.as_bytes());
            strings.push(0);
            table.extend_from_slice(&flags.to_le_bytes());
            table.extend_from_slice(&key.to_le_bytes());
            table.extend_from_slice(&value.to_le_bytes());
            table.extend_from_slice(&0_u32.to_le_bytes());
            table.extend_from_slice(&0_u64.to_le_bytes());
        }
        let mut bytes = LD_CACHE_NEW_MAGIC.to_vec();
        bytes.extend_from_slice(&(entries.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&(strings.len() as u32).to_le_bytes());
        bytes.resize(LD_CACHE_NEW_HEADER, 0);
        bytes.extend_from_slice(&table);
        bytes.extend_from_slice(&strings);
        bytes
    }

    fn old_format_cache(entries: &[(i32, &str, &str)]) -> Vec<u8> {
        let mut strings = Vec::new();
        let mut table = Vec::new();
        for (flags, soname, path) in entries {
            let key = strings.len() as u32;
            strings.extend_from_slice(soname.as_bytes());
            strings.push(0);
            let value = strings.len() as u32;
            strings.extend_from_slice(path.as_bytes());
            strings.push(0);
            table.extend_from_slice(&flags.to_le_bytes());
            table.extend_from_slice(&key.to_le_bytes());
            table.extend_from_slice(&value.to_le_bytes());
        }
        let mut bytes = LD_CACHE_OLD_MAGIC.to_vec();
        bytes.resize(12, 0);
        bytes.extend_from_slice(&(entries.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&table);
        bytes.extend_from_slice(&strings);
        bytes
    }

    fn entry(flags: i32, soname: &str, path: &str) -> LdCacheEntry {
        LdCacheEntry {
            flags,
            soname: soname.to_owned(),
            path: PathBuf::from(path),
        }
    }

    #[test]
    fn ld_so_cache_parses_new_old_and_combined_formats() {
        let entries = [
            (0x0303, "libc.so.6", "/usr/lib/libc.so.6"),
            (0x0003, "libc.so.6", "/usr/lib32/libc.so.6"),
        ];
        let expected = vec![
            entry(0x0303, "libc.so.6", "/usr/lib/libc.so.6"),
            entry(0x0003, "libc.so.6", "/usr/lib32/libc.so.6"),
        ];

        assert_eq!(
            parse_ld_so_cache(&new_format_cache(&entries)),
            Some(expected.clone())
        );
        let old = old_format_cache(&entries);
        assert_eq!(parse_ld_so_cache(&old), Some(expected.clone()));

        // The combined layout appends the new format at the next 8-byte
        // boundary after the old entry table; its strings are relative to
        // the new header.
        let new_only = [(0x0003, "libX11.so.6", "/usr/lib32/libX11.so.6")];
        let mut combined = old_format_cache(&entries[..1]);
        combined.truncate(LD_CACHE_OLD_HEADER + LD_CACHE_OLD_ENTRY);
        combined.resize((combined.len() + 7) & !7, 0);
        combined.extend_from_slice(&new_format_cache(&new_only));
        assert_eq!(
            parse_ld_so_cache(&combined),
            Some(vec![entry(0x0003, "libX11.so.6", "/usr/lib32/libX11.so.6")])
        );
    }

    #[test]
    fn malformed_ld_so_cache_is_rejected_without_panicking() {
        let valid = new_format_cache(&[(0x0003, "libc.so.6", "/usr/lib32/libc.so.6")]);
        for length in 0..valid.len() {
            let _ = parse_ld_so_cache(&valid[..length]);
        }
        assert_eq!(parse_ld_so_cache(&valid[..valid.len() - 1]), None);
        assert_eq!(parse_ld_so_cache(b"not a loader cache"), None);

        let mut out_of_range = valid.clone();
        out_of_range[LD_CACHE_NEW_HEADER + 4..LD_CACHE_NEW_HEADER + 8]
            .copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(parse_ld_so_cache(&out_of_range), None);

        let mut oversized = valid;
        oversized[20..24].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(parse_ld_so_cache(&oversized), None);
    }

    fn elf_fixture(path: &Path, class: u8, machine: u16) {
        let mut header = vec![0x7f, b'E', b'L', b'F', class, 1, 1];
        header.resize(18, 0);
        header.extend_from_slice(&machine.to_le_bytes());
        header.resize(64, 0);
        fs::write(path, header).unwrap();
    }

    #[test]
    fn library_presence_requires_matching_cache_class_and_elf_header() {
        let directory = tempfile::tempdir().unwrap();
        let i386 = directory.path().join("libc.so.6");
        let amd64 = directory.path().join("libunwind.so.8");
        elf_fixture(&i386, 1, 3);
        elf_fixture(&amd64, 2, 62);
        let i386_text = i386.to_str().unwrap();
        let amd64_text = amd64.to_str().unwrap();

        let no_fallback: [&Path; 0] = [];
        assert!(library_present(
            &[entry(0x0003, "libc.so.6", i386_text)],
            "libc.so.6",
            ElfTarget::I386,
            &no_fallback
        ));
        assert!(!library_present(
            &[entry(0x0303, "libc.so.6", i386_text)],
            "libc.so.6",
            ElfTarget::I386,
            &no_fallback
        ));
        assert!(!library_present(
            &[entry(0x0003, "libunwind.so.8", amd64_text)],
            "libunwind.so.8",
            ElfTarget::I386,
            &no_fallback
        ));
        assert!(library_present(
            &[entry(0x0303, "libunwind.so.8", amd64_text)],
            "libunwind.so.8",
            ElfTarget::X86_64,
            &no_fallback
        ));
        // Fallback directories are consulted with the same ELF check.
        assert!(library_present(
            &[],
            "libc.so.6",
            ElfTarget::I386,
            &[directory.path()]
        ));
        assert!(!library_present(
            &[],
            "libunwind.so.8",
            ElfTarget::I386,
            &[directory.path()]
        ));
        assert!(elf_matches(&i386, ElfTarget::I386));
        assert!(!elf_matches(&i386, ElfTarget::X86_64));
        assert!(!elf_matches(directory.path(), ElfTarget::I386));
    }

    #[test]
    fn missing_i386_libraries_names_each_unsatisfied_group() {
        let directory = tempfile::tempdir().unwrap();
        let cache = I386_LIBRARY_REQUIREMENTS
            .iter()
            .map(|group| {
                let path = directory.path().join(group[group.len() - 1]);
                elf_fixture(&path, 1, 3);
                entry(0x0003, group[group.len() - 1], path.to_str().unwrap())
            })
            .filter(|entry| entry.soname != "libgnutls.so.30")
            .collect::<Vec<_>>();

        let no_fallback: [&Path; 0] = [];
        assert_eq!(
            missing_i386_libraries(&cache, &no_fallback),
            ["libgnutls.so.30"]
        );
        assert_eq!(
            missing_i386_libraries(&[], &no_fallback),
            [
                "libc.so.6",
                "libX11.so.6",
                "libGL.so.1",
                "libfreetype.so.6",
                "libfontconfig.so.1",
                "libgnutls.so.30",
                "libasound.so.2 or libpulse.so.0",
            ]
        );
        // Every fixture (only the last alternative of each group) lives in
        // the fallback directory, so nothing is missing through it.
        assert!(missing_i386_libraries(&[], &[directory.path()]).is_empty());
    }

    #[test]
    fn missing_wine_libraries_names_each_absent_64_bit_library() {
        let directory = tempfile::tempdir().unwrap();
        let no_fallback: [&Path; 0] = [];
        let cached = |sonames: &[&str]| {
            sonames
                .iter()
                .map(|soname| {
                    let path = directory.path().join(soname);
                    elf_fixture(&path, 2, 62);
                    entry(0x0303, soname, path.to_str().unwrap())
                })
                .collect::<Vec<_>>()
        };

        assert_eq!(missing_wine_libraries(&[], &no_fallback), WineLibrary::ALL);
        let all_but_opengl = cached(&[
            "libX11.so.6",
            "libXext.so.6",
            "libfreetype.so.6",
            "libfontconfig.so.1",
        ]);
        assert_eq!(
            missing_wine_libraries(&all_but_opengl, &no_fallback),
            [WineLibrary::OpenGl]
        );
        // Wine 11 draws through EGL or, without it, GLX: either one is
        // OpenGL.
        for opengl in ["libEGL.so.1", "libGL.so.1"] {
            let mut cache = all_but_opengl.clone();
            cache.extend(cached(&[opengl]));
            assert!(
                missing_wine_libraries(&cache, &no_fallback).is_empty(),
                "{opengl}"
            );
        }
        assert_eq!(
            missing_wine_libraries(&cached(&["libX11.so.6", "libGL.so.1"]), &no_fallback),
            [
                WineLibrary::Xext,
                WineLibrary::FreeType,
                WineLibrary::Fontconfig
            ]
        );
        assert!(missing_wine_libraries(&[], &[directory.path()]).is_empty());

        // A 32-bit library of the same name does not serve the WoW64 runtime.
        let i386 = tempfile::tempdir().unwrap();
        for library in WineLibrary::ALL {
            for soname in library.sonames() {
                elf_fixture(&i386.path().join(soname), 1, 3);
            }
        }
        assert_eq!(
            missing_wine_libraries(&[], &[i386.path()]),
            WineLibrary::ALL
        );
    }

    #[test]
    fn os_release_fixtures_cover_optional_version_and_quoting() {
        let fedora = parse_os_release(
            "NAME=\"Fedora Linux\"\nVERSION=\"44 (Workstation Edition)\"\nID=fedora\n\
             VERSION_ID=44\nVARIANT_ID=workstation\n",
        );
        assert_eq!(fedora.get("ID").map(String::as_str), Some("fedora"));
        assert_eq!(fedora.get("VERSION_ID").map(String::as_str), Some("44"));
        assert_eq!(
            fedora.get("VARIANT_ID").map(String::as_str),
            Some("workstation")
        );

        let cachyos = parse_os_release(
            "NAME=\"CachyOS Linux\"\nPRETTY_NAME=\"CachyOS\"\nID=cachyos\nID_LIKE=arch\n\
             BUILD_ID=rolling\nANSI_COLOR=\"38;2;23;147;209\"\n",
        );
        assert_eq!(cachyos.get("ID").map(String::as_str), Some("cachyos"));
        assert_eq!(cachyos.get("VERSION_ID"), None);
        assert_eq!(cachyos.get("BUILD_ID").map(String::as_str), Some("rolling"));

        let debian = parse_os_release(
            "# comment\nPRETTY_NAME=\"Debian GNU/Linux 13 (trixie)\"\nVERSION_ID=\"13\"\n\
             ID=debian\nVERSION='13 (trixie)'\nHOME_URL=\"https://www.debian.org/\"\n\
             ESCAPED=\"a \\\"quoted\\\" \\$value\"\n",
        );
        assert_eq!(debian.get("ID").map(String::as_str), Some("debian"));
        assert_eq!(debian.get("VERSION_ID").map(String::as_str), Some("13"));
        assert_eq!(
            debian.get("VERSION").map(String::as_str),
            Some("13 (trixie)")
        );
        assert_eq!(
            debian.get("PRETTY_NAME").map(String::as_str),
            Some("Debian GNU/Linux 13 (trixie)")
        );
        assert_eq!(
            debian.get("ESCAPED").map(String::as_str),
            Some("a \"quoted\" $value")
        );
        assert_eq!(
            parse_os_release("UNTERMINATED=\"value\n").get("UNTERMINATED"),
            None
        );
    }

    #[test]
    fn display_session_uses_display_presence_when_the_session_type_is_tty() {
        assert_eq!(
            classify_display_session(Some("wayland"), false, false),
            DisplaySession::Wayland
        );
        assert_eq!(
            classify_display_session(Some("X11"), false, false),
            DisplaySession::X11
        );
        assert_eq!(
            classify_display_session(Some("tty"), true, true),
            DisplaySession::Wayland
        );
        assert_eq!(
            classify_display_session(Some("tty"), false, true),
            DisplaySession::X11
        );
        assert_eq!(
            classify_display_session(Some("tty"), false, false),
            DisplaySession::Other
        );
        assert_eq!(
            classify_display_session(None, false, false),
            DisplaySession::Unknown
        );
    }

    #[test]
    fn x11_display_requires_a_local_socket_or_a_remote_display() {
        let directory = tempfile::tempdir().unwrap();
        let _listener =
            std::os::unix::net::UnixListener::bind(directory.path().join("X0")).unwrap();
        fs::write(directory.path().join("X1"), b"not a socket").unwrap();

        for (display, expected) in [
            (":0", true),
            (":0.0", true),
            ("unix:0", true),
            (":1", false),
            (":2", false),
            ("remote.example:0", true),
            ("", false),
            ("wayland-0", false),
            (":x", false),
        ] {
            assert_eq!(
                x11_display_available(Some(OsStr::new(display)), directory.path()),
                expected,
                "{display}"
            );
        }
        assert!(!x11_display_available(None, directory.path()));
    }

    #[test]
    fn kernel_command_line_ia32_emulation_follows_kstrtobool() {
        let state = |command_line| kernel_ia32_emulation(Some(command_line));
        for enabled in [
            "root=/dev/sda1 quiet",
            "ia32_emulation=1",
            "ia32_emulation=y",
            "ia32_emulation=Yes",
            "ia32_emulation=true",
            "ia32_emulation=on",
            "ia32_emulation=ON",
            "ia32_emulation=0 ia32_emulation=1",
            "ia32_emulation=off ia32_emulation=on",
            "ia32_emulation=",
            "ia32_emulation=maybe",
            "ia32_emulation=o",
            "quiet -- ia32_emulation=0",
        ] {
            assert_eq!(state(enabled), Ia32Emulation::NotDisabled, "{enabled}");
        }
        for disabled in [
            "quiet ia32_emulation=0",
            "ia32_emulation=n",
            "ia32_emulation=No",
            "ia32_emulation=false",
            "ia32_emulation=F",
            "ia32_emulation=off",
            "ia32_emulation=OFF",
            "ia32_emulation=1 ia32_emulation=0",
            "ia32_emulation=0 ia32_emulation=bogus",
        ] {
            assert_eq!(state(disabled), Ia32Emulation::Disabled, "{disabled}");
        }
        assert_eq!(kernel_ia32_emulation(None), Ia32Emulation::Unknown);
    }

    #[test]
    fn direct_preflight_classifies_host_side_refusals() {
        let blocked = RuntimeExecutionEvidence::Blocked;
        assert_eq!(
            classify_runtime_preflight(
                true,
                &output(
                    127,
                    b"",
                    b"wine: error while loading shared libraries: libX11.so.6: cannot open"
                ),
                || false
            ),
            blocked(GuestExecutionBlocker::SharedLibrariesMissing)
        );
        assert_eq!(
            classify_runtime_preflight(
                true,
                &output(1, b"", b"/lib/ld-linux.so.2: could not open\n"),
                || false
            ),
            blocked(GuestExecutionBlocker::SharedLibrariesMissing)
        );
        assert_eq!(
            classify_runtime_preflight(
                true,
                &output(126, b"", b"cannot execute binary file"),
                || false
            ),
            blocked(GuestExecutionBlocker::Ia32ExecutionDisabled)
        );
        assert_eq!(
            classify_runtime_preflight(true, &output(2, b"", b""), || true),
            blocked(GuestExecutionBlocker::Ia32ExecutionDisabled)
        );
        assert_eq!(
            classify_runtime_preflight(true, &output(1, b"", b"wine failed"), || false),
            RuntimeExecutionEvidence::Failed
        );
        // dash interpreting an ELF after glibc's ENOEXEC fallback.
        for stderr in [
            &b"/proc/self/fd/212: 1: Syntax error: \")\" unexpected\n"[..],
            b"/proc/self/fd/212: 1: \x7fELF\x01\x01\x01: not found\n",
            b"sh: 1: cannot open /proc/self/fd/212\n",
        ] {
            for code in [2, 127] {
                assert_eq!(
                    classify_runtime_preflight(true, &output(code, b"", stderr), || false),
                    RuntimeExecutionEvidence::Unavailable,
                    "{}",
                    String::from_utf8_lossy(stderr)
                );
            }
        }
        assert_eq!(
            classify_runtime_preflight(
                true,
                &output(
                    1,
                    b"",
                    b"wine: could not load kernel32.dll, status c0000135\n"
                ),
                || false
            ),
            RuntimeExecutionEvidence::Failed,
            "a Wine diagnostic remains runtime evidence"
        );
        assert_eq!(
            classify_direct_spawn_error(&io::Error::from_raw_os_error(
                rustix::io::Errno::NOEXEC.raw_os_error()
            )),
            blocked(GuestExecutionBlocker::Ia32ExecutionDisabled)
        );
        assert_eq!(
            classify_direct_spawn_error(&io::Error::from_raw_os_error(
                rustix::io::Errno::ACCESS.raw_os_error()
            )),
            blocked(GuestExecutionBlocker::ExecutionDenied)
        );
        assert_eq!(
            classify_direct_spawn_error(&io::Error::from_raw_os_error(
                rustix::io::Errno::NOENT.raw_os_error()
            )),
            RuntimeExecutionEvidence::Unavailable
        );
    }

    #[test]
    fn translated_preflight_evidence_is_unchanged() {
        assert_eq!(
            classify_runtime_preflight(
                false,
                &output(127, b"", b"error while loading shared libraries"),
                || true
            ),
            RuntimeExecutionEvidence::Failed
        );
        assert_eq!(
            classify_runtime_preflight(
                false,
                &output(
                    0,
                    format!("{}\n", crate::runtime::current().version).as_bytes(),
                    b""
                ),
                || false
            ),
            RuntimeExecutionEvidence::Passed
        );
        assert_eq!(
            classify_runtime_preflight(false, &output(0, b"wine-9.0", b""), || false),
            RuntimeExecutionEvidence::Failed
        );
    }

    #[test]
    fn a_custom_runtime_preflight_accepts_any_wine_version_string() {
        for version in [
            &b"wine-9.0\n"[..],
            b"wine-10.0-rc1 (Staging)",
            b"wine-11.18",
        ] {
            assert_eq!(
                classify_runtime_preflight_for(false, &output(0, version, b""), None, || false),
                RuntimeExecutionEvidence::Passed
            );
        }
        for rejected in [&b""[..], b"not wine", b"bash: wine: command not found"] {
            assert_eq!(
                classify_runtime_preflight_for(false, &output(0, rejected, b""), None, || false),
                RuntimeExecutionEvidence::Failed
            );
        }
    }

    #[test]
    fn vulkan_icd_search_honors_explicit_lists_and_xdg_directories() {
        let explicit = vulkan_icd_search_paths(|name| match name {
            "VK_DRIVER_FILES" => Some(OsString::from("/opt/icd.json:relative.json")),
            "XDG_DATA_DIRS" => Some(OsString::from("/ignored")),
            _ => None,
        });
        assert_eq!(explicit, [PathBuf::from("/opt/icd.json")]);

        let legacy = vulkan_icd_search_paths(|name| {
            (name == "VK_ICD_FILENAMES").then(|| OsString::from("/opt/legacy.json"))
        });
        assert_eq!(legacy, [PathBuf::from("/opt/legacy.json")]);

        let standard = vulkan_icd_search_paths(|name| match name {
            "HOME" => Some(OsString::from("/home/player")),
            "XDG_DATA_DIRS" => Some(OsString::from("/nix/profile/share:/usr/share")),
            _ => None,
        });
        for expected in [
            "/home/player/.config/vulkan/icd.d",
            "/etc/xdg/vulkan/icd.d",
            "/etc/vulkan/icd.d",
            "/home/player/.local/share/vulkan/icd.d",
            "/nix/profile/share/vulkan/icd.d",
            "/usr/share/vulkan/icd.d",
            "/run/opengl-driver/share/vulkan/icd.d",
        ] {
            assert!(
                standard.contains(&PathBuf::from(expected)),
                "{expected} missing from {standard:?}"
            );
        }
    }

    #[test]
    fn explicit_icd_file_is_validated_like_a_directory_entry() {
        let directory = tempfile::tempdir().unwrap();
        let icd = directory.path().join("vendor.json");
        fs::write(&icd, r#"{"ICD":{"library_path":"libvulkan_vendor.so"}}"#).unwrap();
        assert!(vulkan_icd_path_present(&icd));
        fs::write(&icd, b"{}").unwrap();
        assert!(!vulkan_icd_path_present(&icd));
        assert!(!vulkan_icd_path_present(&directory.path().join("absent")));
    }
}
