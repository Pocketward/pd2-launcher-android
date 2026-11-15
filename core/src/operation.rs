//! Typed cooperative control shared by mutating workflows.
//!
//! The progress channel is deliberately a single coalescing slot: producers
//! replace stale intermediate samples instead of blocking a mutation or
//! growing memory when the GPUI consumer is busy.

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum WorkPhase {
    RuntimeAcquisition,
    PrefixInitialization,
    BaseGameInstall,
    ExpansionInstall,
    /// Blizzard's downloader fetching the Diablo II installer (no key).
    BaseGameDownload,
    /// Blizzard's downloader fetching the Lord of Destruction installer.
    ExpansionDownload,
    /// Copying or recording a Diablo II the user already has.
    ImportGame,
    ClientManifest,
    ClientDownload,
    Hashing,
    Promotion,
    Repair,
    Verification,
    /// The official PD2 launcher is open under the Launcher's lease.
    OfficialLauncher,
    /// A confirmed Start over removes the Managed prefix.
    Reset,
    /// Choosing or changing the install location.
    Relocation,
}

/// One part of setting up the game environment
/// ([`WorkPhase::PrefixInitialization`]), in the order the parts run.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum EnvironmentPart {
    /// The Wine runtime unpacked and the Managed prefix made by Wine's first
    /// start, with the registry values the Launcher sets in it.
    Windows,
    /// Wine Gecko.
    WebComponents,
    /// Wine Mono.
    Dotnet,
    /// The .NET Desktop Runtime the official PD2 launcher needs.
    LauncherSupport,
}

impl EnvironmentPart {
    pub(crate) const ALL: [Self; 4] = [
        Self::Windows,
        Self::WebComponents,
        Self::Dotnet,
        Self::LauncherSupport,
    ];

    const fn bit(self) -> u8 {
        1 << self as u8
    }
}

/// Which parts one setup of the game environment runs, and the one running.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct EnvironmentProgress {
    running: EnvironmentPart,
    planned: u8,
}

impl EnvironmentProgress {
    /// `running`, one of `planned` (it is added when missing).
    pub(crate) fn new(running: EnvironmentPart, planned: &[EnvironmentPart]) -> Self {
        Self {
            running,
            planned: planned
                .iter()
                .fold(running.bit(), |bits, part| bits | part.bit()),
        }
    }

    pub(crate) const fn running(self) -> EnvironmentPart {
        self.running
    }

    /// The planned parts, in the order they run.
    pub(crate) fn planned(self) -> impl Iterator<Item = EnvironmentPart> {
        EnvironmentPart::ALL
            .into_iter()
            .filter(move |part| self.planned & part.bit() != 0)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum ProgressUnit {
    Bytes,
    Files,
    Stages,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum CancellationSafety {
    NotRequested = 0,
    Requested = 1,
    ProvenSafe = 2,
    Unproven = 3,
}

impl WorkPhase {
    pub(crate) const fn message(self) -> &'static str {
        match self {
            Self::RuntimeAcquisition => "Acquiring managed runtime",
            Self::PrefixInitialization => "Initializing managed Wine prefix",
            Self::BaseGameInstall => "Installing Diablo II",
            Self::ExpansionInstall => "Installing Lord of Destruction",
            Self::BaseGameDownload => "Downloading the Diablo II installer",
            Self::ExpansionDownload => "Downloading the Lord of Destruction installer",
            Self::ImportGame => "Importing an existing Diablo II installation",
            Self::ClientManifest => "Checking the PD2 client manifest",
            Self::ClientDownload => "Downloading verified PD2 client files",
            Self::Hashing => "Verifying downloaded content",
            Self::Promotion => "Committing verified client files",
            Self::Repair => "Repairing an interrupted transaction",
            Self::Verification => "Auditing durable installation evidence",
            Self::OfficialLauncher => "Running the official PD2 launcher",
            Self::Reset => "Removing the Managed prefix to start over",
            Self::Relocation => "Changing the install location",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ProgressDimension {
    unit: ProgressUnit,
    completed: u64,
    total: Option<u64>,
}

impl ProgressDimension {
    pub(crate) fn new(unit: ProgressUnit, completed: u64, total: Option<u64>) -> Self {
        let total = total.map(|total| total.max(1));
        Self {
            unit,
            completed: total.map_or(completed, |total| completed.min(total)),
            total,
        }
    }

    pub(crate) const fn unit(self) -> ProgressUnit {
        self.unit
    }

    pub(crate) const fn completed(self) -> u64 {
        self.completed
    }

    pub(crate) const fn total(self) -> Option<u64> {
        self.total
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct WorkProgress {
    phase: WorkPhase,
    file_bytes: Option<ProgressDimension>,
    overall: Option<ProgressDimension>,
    verification: Option<ProgressDimension>,
    environment: Option<EnvironmentProgress>,
}

impl WorkProgress {
    pub(crate) const fn phase(self) -> WorkPhase {
        self.phase
    }

    /// The game environment's part being set up, in
    /// [`WorkPhase::PrefixInitialization`].
    pub(crate) const fn environment(self) -> Option<EnvironmentProgress> {
        self.environment
    }

    pub(crate) const fn with_environment(mut self, environment: EnvironmentProgress) -> Self {
        self.environment = Some(environment);
        self
    }

    #[cfg(test)]
    pub(crate) const fn completed(self) -> Option<u64> {
        match self.primary() {
            Some(progress) => Some(progress.completed()),
            None => None,
        }
    }

    #[cfg(test)]
    pub(crate) const fn unit(self) -> Option<ProgressUnit> {
        match self.primary() {
            Some(progress) => Some(progress.unit()),
            None => None,
        }
    }

    #[cfg(test)]
    pub(crate) const fn total(self) -> Option<u64> {
        match self.primary() {
            Some(progress) => progress.total(),
            None => None,
        }
    }

    pub(crate) const fn file_bytes(self) -> Option<ProgressDimension> {
        self.file_bytes
    }

    pub(crate) const fn overall(self) -> Option<ProgressDimension> {
        self.overall
    }

    pub(crate) const fn verification(self) -> Option<ProgressDimension> {
        self.verification
    }

    #[cfg(test)]
    const fn primary(self) -> Option<ProgressDimension> {
        match self.file_bytes {
            Some(progress) => Some(progress),
            None => match self.overall {
                Some(progress) => Some(progress),
                None => self.verification,
            },
        }
    }

    pub(crate) const fn for_phase(phase: WorkPhase) -> Self {
        Self {
            phase,
            file_bytes: None,
            overall: None,
            verification: None,
            environment: None,
        }
    }

    pub(crate) fn bounded(
        phase: WorkPhase,
        unit: ProgressUnit,
        completed: u64,
        total: u64,
    ) -> Self {
        let progress = ProgressDimension::new(unit, completed, Some(total));
        match unit {
            ProgressUnit::Bytes => Self::for_phase(phase).with_file_bytes_dimension(progress),
            ProgressUnit::Files | ProgressUnit::Stages => {
                Self::for_phase(phase).with_overall_dimension(progress)
            }
        }
    }

    #[cfg(test)]
    pub(crate) const fn indeterminate(
        phase: WorkPhase,
        unit: ProgressUnit,
        completed: u64,
    ) -> Self {
        let progress = ProgressDimension {
            unit,
            completed,
            total: None,
        };
        match unit {
            ProgressUnit::Bytes => Self::for_phase(phase).with_file_bytes_dimension(progress),
            ProgressUnit::Files | ProgressUnit::Stages => {
                Self::for_phase(phase).with_overall_dimension(progress)
            }
        }
    }

    pub(crate) fn with_file_bytes(mut self, completed: u64, total: Option<u64>) -> Self {
        self.file_bytes = Some(ProgressDimension::new(
            ProgressUnit::Bytes,
            completed,
            total,
        ));
        self
    }

    pub(crate) fn with_overall(
        mut self,
        unit: ProgressUnit,
        completed: u64,
        total: Option<u64>,
    ) -> Self {
        debug_assert!(matches!(unit, ProgressUnit::Files | ProgressUnit::Stages));
        self.overall = Some(ProgressDimension::new(unit, completed, total));
        self
    }

    pub(crate) fn with_verification(mut self, completed: u64, total: Option<u64>) -> Self {
        self.verification = Some(ProgressDimension::new(
            ProgressUnit::Files,
            completed,
            total,
        ));
        self
    }

    const fn with_file_bytes_dimension(mut self, progress: ProgressDimension) -> Self {
        self.file_bytes = Some(progress);
        self
    }

    const fn with_overall_dimension(mut self, progress: ProgressDimension) -> Self {
        self.overall = Some(progress);
        self
    }
}

#[derive(Clone, Default)]
pub(crate) struct ProgressPublisher(Arc<Mutex<Option<WorkProgress>>>);

pub(crate) struct ProgressReceiver(Arc<Mutex<Option<WorkProgress>>>);

pub(crate) fn progress_channel() -> (ProgressPublisher, ProgressReceiver) {
    let slot = Arc::new(Mutex::new(None));
    (ProgressPublisher(Arc::clone(&slot)), ProgressReceiver(slot))
}

impl ProgressPublisher {
    fn publish(&self, progress: WorkProgress) {
        let mut slot = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *slot = Some(progress);
    }
}

impl ProgressReceiver {
    pub(crate) fn take_latest(&self) -> Option<WorkProgress> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
    }
}

#[derive(Clone, Default)]
pub(crate) struct OperationControl {
    cancelled: Arc<AtomicBool>,
    cancellation_safety: Arc<std::sync::atomic::AtomicU8>,
    progress: Option<ProgressPublisher>,
    observers: Arc<Mutex<Vec<ProgressObserver>>>,
    /// The job's own count of its steps (such as "download 3 of 8"),
    /// which replaces a stage count an adapter reports on its own.
    step: Arc<Mutex<Option<(u64, u64)>>>,
}

type ProgressObserver = Arc<dyn Fn(WorkProgress) + Send + Sync>;

impl OperationControl {
    pub(crate) fn with_progress(progress: ProgressPublisher) -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
            cancellation_safety: Arc::new(std::sync::atomic::AtomicU8::new(
                CancellationSafety::NotRequested as u8,
            )),
            progress: Some(progress),
            observers: Arc::new(Mutex::new(Vec::new())),
            step: Arc::new(Mutex::new(None)),
        }
    }

    /// Count the job's own steps: `completed` of `total` are done.
    pub(crate) fn set_step(&self, completed: u64, total: u64) {
        *self
            .step
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some((completed, total));
    }

    pub(crate) fn request_cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        let _ = self.cancellation_safety.compare_exchange(
            CancellationSafety::NotRequested as u8,
            CancellationSafety::Requested as u8,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    pub(crate) fn is_requested(&self) -> bool {
        self.is_cancelled()
    }

    pub(crate) fn cancellation_safety(&self) -> CancellationSafety {
        match self.cancellation_safety.load(Ordering::Acquire) {
            0 => CancellationSafety::NotRequested,
            1 => CancellationSafety::Requested,
            2 => CancellationSafety::ProvenSafe,
            _ => CancellationSafety::Unproven,
        }
    }

    pub(crate) fn mark_cancelled_safe(&self) {
        if self.is_cancelled() && self.cancellation_safety() != CancellationSafety::Unproven {
            self.cancellation_safety
                .store(CancellationSafety::ProvenSafe as u8, Ordering::Release);
        }
    }

    pub(crate) fn mark_cancel_unproven(&self) {
        if self.is_cancelled() {
            self.cancellation_safety
                .store(CancellationSafety::Unproven as u8, Ordering::Release);
        }
    }

    pub(crate) fn report(&self, mut progress: WorkProgress) {
        let step = *self
            .step
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some((completed, total)) = step
            && progress
                .overall()
                .is_none_or(|overall| overall.unit() == ProgressUnit::Stages)
        {
            progress = progress.with_overall(ProgressUnit::Stages, completed, Some(total));
        }
        if let Some(publisher) = &self.progress {
            publisher.publish(progress);
        }
        let observers = self
            .observers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        for observer in observers {
            observer(progress);
        }
    }

    pub(crate) fn observe(&self, observer: ProgressObserver) {
        self.observers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(observer);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn progress_channel_is_bounded_and_coalesces_to_the_latest_sample() {
        let (publisher, receiver) = progress_channel();
        let control = OperationControl::with_progress(publisher);
        control.report(WorkProgress::bounded(
            WorkPhase::ClientDownload,
            ProgressUnit::Bytes,
            1,
            10,
        ));
        control.report(WorkProgress::bounded(
            WorkPhase::ClientDownload,
            ProgressUnit::Bytes,
            7,
            10,
        ));

        assert_eq!(
            receiver.take_latest(),
            Some(WorkProgress::bounded(
                WorkPhase::ClientDownload,
                ProgressUnit::Bytes,
                7,
                10,
            ))
        );
        assert_eq!(receiver.take_latest(), None);
    }

    #[test]
    fn typed_observers_receive_the_same_progress_as_the_ui_channel() {
        let (publisher, receiver) = progress_channel();
        let control = OperationControl::with_progress(publisher);
        let observations = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&observations);
        control.observe(Arc::new(move |progress| {
            if progress
                == WorkProgress::bounded(WorkPhase::ClientDownload, ProgressUnit::Bytes, 4, 9)
            {
                observed.fetch_add(1, Ordering::AcqRel);
            }
        }));

        control.report(WorkProgress::bounded(
            WorkPhase::ClientDownload,
            ProgressUnit::Bytes,
            4,
            9,
        ));
        assert_eq!(observations.load(Ordering::Acquire), 1);
        assert_eq!(
            receiver.take_latest(),
            Some(WorkProgress::bounded(
                WorkPhase::ClientDownload,
                ProgressUnit::Bytes,
                4,
                9,
            ))
        );
    }

    #[test]
    fn cancellation_safety_is_typed_and_unproven_is_sticky() {
        let control = OperationControl::default();
        control.request_cancel();
        assert_eq!(control.cancellation_safety(), CancellationSafety::Requested);
        control.mark_cancelled_safe();
        assert_eq!(
            control.cancellation_safety(),
            CancellationSafety::ProvenSafe
        );
        control.mark_cancel_unproven();
        control.mark_cancelled_safe();
        assert_eq!(control.cancellation_safety(), CancellationSafety::Unproven);
    }

    #[test]
    fn an_environment_setup_lists_its_parts_in_the_order_they_run_with_the_running_one() {
        let setup = EnvironmentProgress::new(
            EnvironmentPart::Dotnet,
            &[
                EnvironmentPart::LauncherSupport,
                EnvironmentPart::WebComponents,
            ],
        );
        assert_eq!(setup.running(), EnvironmentPart::Dotnet);
        assert_eq!(
            setup.planned().collect::<Vec<_>>(),
            [
                EnvironmentPart::WebComponents,
                EnvironmentPart::Dotnet,
                EnvironmentPart::LauncherSupport,
            ]
        );
        let progress = WorkProgress::for_phase(WorkPhase::PrefixInitialization);
        assert_eq!(progress.environment(), None);
        assert_eq!(progress.with_environment(setup).environment(), Some(setup));
    }

    #[test]
    fn unknown_byte_total_remains_indeterminate_instead_of_becoming_complete() {
        let progress =
            WorkProgress::indeterminate(WorkPhase::ClientDownload, ProgressUnit::Bytes, 64 * 1024);
        assert_eq!(progress.unit(), Some(ProgressUnit::Bytes));
        assert_eq!(progress.completed(), Some(64 * 1024));
        assert_eq!(progress.total(), None);
    }

    #[test]
    fn composite_progress_keeps_file_overall_and_verification_dimensions_together() {
        let progress = WorkProgress::for_phase(WorkPhase::ClientDownload)
            .with_file_bytes(4096, None)
            .with_overall(ProgressUnit::Files, 2, Some(8))
            .with_verification(1, Some(8));

        assert_eq!(
            progress.file_bytes(),
            Some(ProgressDimension::new(ProgressUnit::Bytes, 4096, None))
        );
        assert_eq!(
            progress.overall(),
            Some(ProgressDimension::new(ProgressUnit::Files, 2, Some(8)))
        );
        assert_eq!(
            progress.verification(),
            Some(ProgressDimension::new(ProgressUnit::Files, 1, Some(8)))
        );
    }
}
