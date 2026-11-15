//! Journaled filesystem transaction primitives for update and Repair.

use std::{
    error::Error,
    ffi::OsStr,
    fmt,
    fs::{self, File},
    io::{self, Read},
    path::{Path, PathBuf},
    sync::Arc,
};

#[cfg(test)]
use std::os::unix::fs::MetadataExt;

use md5::{Digest as _, Md5};
use rustix::fs::{Mode, OFlags, open};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

use crate::{
    RelativeGamePath,
    audit::{TransactionEvidence, TransactionProbe},
    manifest::{ManifestResponse, PD2_MANIFEST_URL, Pd2Manifest, hex},
    operation::{OperationControl, ProgressUnit, WorkPhase, WorkProgress},
    safe_fs::{AtomicIntentKind, AtomicIntentSnapshot, AtomicMutation, Directory, FileIdentity},
};

const JOURNAL_SCHEMA: u32 = 1;
const MAX_JOURNAL_BYTES: u64 = 4 * 1024 * 1024;
const STAGING_STATE_SCHEMA: u32 = 3;
const STAGING_CONTAINER: &str = "active";
const STAGING_MAP_A: &str = "generation-map-a.json";
const STAGING_MAP_B: &str = "generation-map-b.json";
const STAGING_MANIFEST: &str = "manifest.json";
const STAGING_OBJECT_PREFIX: &str = "object-";
const STAGING_PARTIAL_PREFIX: &str = "partial-";
const STAGING_MAP_LIMIT: u64 = 4 * 1024 * 1024;
const TRANSACTION_JOURNAL_SLOT: &str = "active.json";
const TRANSACTION_BACKUP_SLOT: &str = "active";
const BACKUP_OBJECT_PREFIX: &str = "object-";

#[derive(Clone)]
pub(crate) struct UpdateLayout {
    project_root: PathBuf,
    anchor: Option<Arc<Directory>>,
    relative_to_anchor: Option<PathBuf>,
}

impl fmt::Debug for UpdateLayout {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UpdateLayout")
            .field("project_root", &self.project_root)
            .field("descriptor_anchored", &self.anchor.is_some())
            .finish()
    }
}

impl UpdateLayout {
    #[cfg(test)]
    pub(crate) fn new(project_root: impl Into<PathBuf>) -> Self {
        Self {
            project_root: project_root.into(),
            anchor: None,
            relative_to_anchor: None,
        }
    }

    /// Binds every security-relevant lookup to `anchor`. The absolute path is
    /// retained only for diagnostics and must not be reopened by operations.
    pub(crate) fn anchored(
        project_root: impl Into<PathBuf>,
        anchor: Directory,
        relative_to_anchor: impl Into<PathBuf>,
    ) -> Self {
        Self {
            project_root: project_root.into(),
            anchor: Some(Arc::new(anchor)),
            relative_to_anchor: Some(relative_to_anchor.into()),
        }
    }

    pub(crate) fn open_project(&self) -> io::Result<Directory> {
        match (&self.anchor, &self.relative_to_anchor) {
            (Some(anchor), Some(relative)) => anchor.open_directory(relative),
            (None, None) => Directory::open(&self.project_root),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "incomplete update layout capability",
            )),
        }
    }

    #[cfg(test)]
    pub(crate) fn metadata_root(&self) -> PathBuf {
        self.project_root.join(".pd2launcher")
    }

    #[cfg(test)]
    pub(crate) fn transactions_directory(&self) -> PathBuf {
        self.metadata_root().join("transactions")
    }

    #[cfg(test)]
    pub(crate) fn staged_files(&self, manifest_id: &str) -> PathBuf {
        let _ = manifest_id;
        self.metadata_root()
            .join("staging")
            .join(STAGING_CONTAINER)
            .join("files")
    }

    #[cfg(test)]
    pub(crate) fn partial_files(&self, manifest_id: &str) -> PathBuf {
        let _ = manifest_id;
        self.metadata_root()
            .join("staging")
            .join(STAGING_CONTAINER)
            .join(".partial")
    }

    #[cfg(test)]
    fn journal_path(&self, transaction_id: &str) -> PathBuf {
        let _ = transaction_id;
        self.transactions_directory().join(TRANSACTION_JOURNAL_SLOT)
    }

    #[cfg(test)]
    fn backup_root(&self, transaction_id: &str) -> PathBuf {
        let _ = transaction_id;
        self.metadata_root()
            .join("backups")
            .join(TRANSACTION_BACKUP_SLOT)
    }

    #[cfg(test)]
    pub(crate) fn accepted_manifest(&self) -> PathBuf {
        self.metadata_root().join("accepted-manifest.json")
    }

    pub(crate) fn open_accepted_manifest(&self) -> io::Result<Option<File>> {
        let project = match self.open_project() {
            Ok(project) => project,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let metadata = match project.open_directory(Path::new(".pd2launcher")) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        match metadata.open_file(Path::new("accepted-manifest.json")) {
            Ok(file) => Ok(Some(file)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    #[cfg(test)]
    pub(crate) fn prepare_staging(&self, manifest: &Pd2Manifest) -> Result<(), TransactionError> {
        validate_identifier(manifest.id())?;
        let directories = prepare_layout(self)?;
        prepare_staging_container(&directories.staging, "test-download", manifest)
    }

    pub(crate) fn acquire_exclusive(&self) -> Result<ExclusiveLease, TransactionError> {
        ExclusiveLease::acquire(prepare_layout(self)?)
    }

    pub(crate) fn prepare_staging_with_lease(
        &self,
        transaction_id: &str,
        manifest: &Pd2Manifest,
        lease: &ExclusiveLease,
    ) -> Result<(), TransactionError> {
        validate_identifier(transaction_id)?;
        validate_identifier(manifest.id())?;
        prepare_staging_container(&lease.directories.staging, transaction_id, manifest)
    }

    pub(crate) fn mark_staging_ready_for_journal(
        &self,
        transaction_id: &str,
        manifest: &Pd2Manifest,
        lease: &ExclusiveLease,
    ) -> Result<(), TransactionError> {
        let staging = lease
            .directories
            .staging
            .open_directory(Path::new(STAGING_CONTAINER))
            .map_err(map_safe_io)?;
        let mut state = read_staging_state(&staging)?;
        require_staging_manifest(&state, manifest)?;
        if state.phase
            != (StagingPhase::Downloading {
                transaction_id: transaction_id.to_owned(),
            })
        {
            return Err(TransactionError::Indeterminate);
        }
        state.generation = state
            .generation
            .checked_add(1)
            .ok_or(TransactionError::InvalidJournal)?;
        state.phase = StagingPhase::ReadyForJournal {
            transaction_id: transaction_id.to_owned(),
        };
        write_staging_state(&staging, &state)
    }

    pub(crate) fn discard_unjournaled_staging(
        &self,
        manifest: &Pd2Manifest,
        lease: &ExclusiveLease,
    ) -> Result<(), TransactionError> {
        let manifest_id = manifest.id();
        validate_identifier(manifest_id)?;
        let staging = match lease
            .directories
            .staging
            .open_directory(Path::new(STAGING_CONTAINER))
        {
            Ok(staging) => staging,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(map_safe_io(error)),
        };
        let mut state = read_staging_state(&staging)?;
        if state.manifest_id != manifest_id || !state.phase.is_download_intent() {
            return Err(TransactionError::Indeterminate);
        }
        if read_journals_from(&lease.directories.transactions)?
            .iter()
            .any(|journal| staging_correlates_with_active_journal(&state, journal))
        {
            return Err(TransactionError::Indeterminate);
        }
        reclaim_staging(&staging, &state, Some(manifest))?;
        state.phase = StagingPhase::Inactive;
        state.generation = state
            .generation
            .checked_add(1)
            .ok_or(TransactionError::InvalidJournal)?;
        write_staging_state(&staging, &state)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct StagingState {
    schema: u32,
    generation: u64,
    manifest_id: String,
    manifest_sha256: String,
    phase: StagingPhase,
    slot_count: usize,
    slot_identities: Vec<StagingSlotIdentity>,
    entries: Vec<StagingEntry>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct StagingSlotIdentity {
    object: FileIdentity,
    partial: FileIdentity,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum StagingPhase {
    Inactive,
    Growing { old_slots: usize, new_slots: usize },
    Downloading { transaction_id: String },
    ReadyForJournal { transaction_id: String },
}

impl StagingPhase {
    fn is_download_intent(&self) -> bool {
        matches!(
            self,
            Self::Downloading { .. } | Self::ReadyForJournal { .. }
        )
    }

    fn transaction_id(&self) -> Option<&str> {
        match self {
            Self::Downloading { transaction_id } | Self::ReadyForJournal { transaction_id } => {
                Some(transaction_id)
            }
            Self::Inactive | Self::Growing { .. } => None,
        }
    }
}

#[cfg(test)]
fn bind_test_staging_transaction(
    staging_root: &Directory,
    transaction_id: &str,
    manifest: &Pd2Manifest,
) -> Result<(), TransactionError> {
    let staging = staging_root
        .open_directory(Path::new(STAGING_CONTAINER))
        .map_err(map_safe_io)?;
    let mut state = read_staging_state(&staging)?;
    require_staging_manifest(&state, manifest)?;
    match &state.phase {
        StagingPhase::Downloading { .. } => {
            state.phase = StagingPhase::ReadyForJournal {
                transaction_id: transaction_id.to_owned(),
            };
            state.generation = state
                .generation
                .checked_add(1)
                .ok_or(TransactionError::InvalidJournal)?;
            write_staging_state(&staging, &state)
        }
        StagingPhase::ReadyForJournal {
            transaction_id: current,
        } if current == transaction_id => Ok(()),
        _ => Err(TransactionError::Indeterminate),
    }
}

fn staging_correlates_with_active_journal(state: &StagingState, journal: &Journal) -> bool {
    if journal.state.is_terminal() || journal.manifest_id != state.manifest_id {
        return false;
    }
    match &state.phase {
        StagingPhase::Downloading { transaction_id }
        | StagingPhase::ReadyForJournal { transaction_id } => {
            journal.transaction_id == *transaction_id
        }
        StagingPhase::Inactive | StagingPhase::Growing { .. } => false,
    }
}

fn staging_is_claimed_by_journal(state: &StagingState, journal: &Journal) -> bool {
    staging_correlates_with_active_journal(state, journal)
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct StagingEntry {
    path: String,
    md5: String,
    object_slot: String,
    partial_slot: String,
}

#[derive(Deserialize, Serialize)]
struct StagingEnvelope {
    state: StagingState,
    checksum: String,
}

struct LayoutDirectories {
    project: Directory,
    metadata: Directory,
    transactions: Directory,
    staging: Directory,
    backups: Directory,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FaultEvent {
    JournalWrite,
    FilesystemMutation,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AtomicSurface {
    Project,
    Backup,
    Journal,
}

pub(crate) trait FaultInjector {
    fn after(&self, event: FaultEvent) -> io::Result<()>;

    fn atomic_after(&self, _surface: AtomicSurface, _event: AtomicMutation) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct NoFaults;

impl FaultInjector for NoFaults {
    fn after(&self, _event: FaultEvent) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Promotion {
    pub(crate) path: RelativeGamePath,
    pub(crate) expected_md5: [u8; 16],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PromotionOutcome {
    Committed,
}

/// Capability supplied only by a just-completed audit whose transaction check
/// was satisfied. Cleanup never infers safety from an old journal state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PruneAuditEvidence {
    FreshAndClear,
}

/// One OS-backed exclusion domain shared by diagnostic export and terminal
/// transaction pruning. Holding this guard leaves no check/release window
/// between observing export state and deleting retained evidence.
pub(crate) struct CleanupLease(File);

impl CleanupLease {
    pub(crate) fn acquire(path: &Path) -> Result<Self, TransactionError> {
        reject_symlink_if_present(path)?;
        let file = open(
            path,
            OFlags::CREATE | OFlags::RDWR | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
        )
        .map(File::from)
        .map_err(io::Error::from)?;
        if !file.metadata()?.is_file() {
            return Err(TransactionError::UnsafePath);
        }
        File::lock(&file)?;
        Ok(Self(file))
    }
}

impl Drop for CleanupLease {
    fn drop(&mut self) {
        let _ = File::unlock(&self.0);
    }
}

pub(crate) struct TransactionManager<'a> {
    layout: &'a UpdateLayout,
    faults: &'a dyn FaultInjector,
}

impl<'a> TransactionManager<'a> {
    pub(crate) fn new(layout: &'a UpdateLayout, faults: &'a dyn FaultInjector) -> Self {
        Self { layout, faults }
    }

    #[cfg(test)]
    pub(crate) fn promote(
        &self,
        transaction_id: &str,
        manifest: &Pd2Manifest,
        promotions: &[Promotion],
    ) -> Result<PromotionOutcome, TransactionError> {
        validate_identifier(transaction_id)?;
        let lease = ExclusiveLease::acquire(prepare_layout(self.layout)?)?;
        self.promote_with_lease(transaction_id, manifest, promotions, &lease)
    }

    #[cfg(test)]
    pub(crate) fn promote_with_lease(
        &self,
        transaction_id: &str,
        manifest: &Pd2Manifest,
        promotions: &[Promotion],
        lease: &ExclusiveLease,
    ) -> Result<PromotionOutcome, TransactionError> {
        self.promote_with_lease_controlled(
            transaction_id,
            manifest,
            promotions,
            lease,
            &OperationControl::default(),
        )
    }

    pub(crate) fn promote_with_lease_controlled(
        &self,
        transaction_id: &str,
        manifest: &Pd2Manifest,
        promotions: &[Promotion],
        lease: &ExclusiveLease,
        control: &OperationControl,
    ) -> Result<PromotionOutcome, TransactionError> {
        reject_cancelled(control)?;
        validate_identifier(transaction_id)?;
        #[cfg(test)]
        bind_test_staging_transaction(&lease.directories.staging, transaction_id, manifest)?;
        let directories = &lease.directories;
        if !matches!(inspect_pending_journal(directories)?, PendingJournal::Clear) {
            return Err(TransactionError::Indeterminate);
        }
        let project_directory = &directories.project;

        let actions = promotions
            .iter()
            .enumerate()
            .map(|(index, promotion)| {
                let previous_md5 =
                    match project_directory.open_file(Path::new(promotion.path.as_str())) {
                        Ok(mut file) => Some(hex(&file_md5_open(&mut file)?)),
                        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
                        Err(error) => return Err(TransactionError::Io(error)),
                    };
                Ok(ActionRecord {
                    path: promotion.path.as_str().to_owned(),
                    expected_md5: hex(&promotion.expected_md5),
                    previous_md5,
                    backup_slot: Some(backup_object_name(index)),
                    backed_up: false,
                    promoted: false,
                })
            })
            .collect::<Result<Vec<_>, TransactionError>>()?;

        let mut journal = Journal {
            schema: JOURNAL_SCHEMA,
            publication_generation: 0,
            transaction_id: transaction_id.to_owned(),
            manifest_id: manifest.id().to_owned(),
            manifest_sha256: Some(hex(&Sha256::digest(manifest.raw()))),
            state: JournalState::Planned,
            backup_slot_count: 0,
            backup_identities: Vec::new(),
            backup_growth: None,
            actions,
        };
        let backup_directory = directories
            .backups
            .ensure_directory(Path::new(TRANSACTION_BACKUP_SLOT))
            .map_err(map_safe_io)?;
        let observed_backup_slots = validate_backup_inventory(&backup_directory)?;
        require_all_backup_slots_empty(&backup_directory)?;
        let prior = read_journals_from(&directories.transactions)?;
        let observed_backup_identities =
            collect_backup_identities(&backup_directory, observed_backup_slots)?;
        match prior.as_slice() {
            [] if observed_backup_slots == 0 => {}
            // Every slot is proven empty above, so on the Android host a
            // pruned journal's slot identities vouch for nothing a copied
            // folder changes; the new journal records the observed ones.
            [prior]
                if matches!(prior.state, JournalState::Pruned)
                    && prior.backup_slot_count == observed_backup_slots
                    && (copied_idle_slots_accepted()
                        || prior.backup_identities == observed_backup_identities) => {}
            _ => return Err(TransactionError::Indeterminate),
        }
        let desired_backup_slots = observed_backup_slots.max(journal.actions.len());
        journal.backup_slot_count = observed_backup_slots;
        if desired_backup_slots > observed_backup_slots {
            journal.backup_growth = Some(BackupGrowth {
                old_slots: observed_backup_slots,
                new_slots: desired_backup_slots,
                identities: observed_backup_identities.clone(),
            });
        }
        journal.backup_identities = observed_backup_identities;
        // The journal is the redundant atomic capacity intent and must precede
        // every new backup slot.
        self.write_journal(directories, &mut journal)?;
        for index in observed_backup_slots..desired_backup_slots {
            let created = backup_directory
                .open_or_create_file(OsStr::new(&backup_object_name(index)))
                .map_err(map_safe_io)?;
            backup_directory.sync().map_err(map_safe_io)?;
            let identity = FileIdentity::of(&created).map_err(map_safe_io)?;
            journal.backup_identities.push(identity);
            if let Some(growth) = journal.backup_growth.as_mut() {
                growth.identities.push(identity);
            }
            self.write_journal(directories, &mut journal)?;
            self.faults.after(FaultEvent::FilesystemMutation)?;
        }
        backup_directory.sync().map_err(map_safe_io)?;
        self.faults.after(FaultEvent::FilesystemMutation)?;
        journal.backup_slot_count = desired_backup_slots;
        journal.backup_growth = None;
        self.write_journal(directories, &mut journal)?;
        let staged_directory = directories
            .staging
            .open_directory(Path::new(STAGING_CONTAINER).join("files").as_path())
            .map_err(map_safe_io)?;
        let staging_state = read_staging_state(
            &directories
                .staging
                .open_directory(Path::new(STAGING_CONTAINER))
                .map_err(map_safe_io)?,
        )?;
        require_staging_manifest(&staging_state, manifest)?;

        for index in 0..journal.actions.len() {
            control.report(WorkProgress::bounded(
                WorkPhase::Promotion,
                ProgressUnit::Files,
                index as u64,
                journal.actions.len().max(1) as u64,
            ));
            let relative = RelativeGamePath::parse(&journal.actions[index].path)
                .map_err(|_| TransactionError::InvalidJournal)?;
            let relative_name = OsStr::new(relative.as_str());

            journal.state = JournalState::BackingUp { index };
            self.write_journal(directories, &mut journal)?;
            if let Some(previous) = &journal.actions[index].previous_md5 {
                let backup_name = journal.actions[index]
                    .backup_slot
                    .as_deref()
                    .ok_or(TransactionError::InvalidJournal)?;
                let mut previous_file = project_directory
                    .open_file(Path::new(relative.as_str()))
                    .map_err(map_safe_io)?;
                verify_open_md5(&mut previous_file, previous)?;
                backup_directory
                    .atomic_write_from_open_recoverable(
                        OsStr::new(backup_name),
                        &previous_file,
                        &mut |event| self.faults.atomic_after(AtomicSurface::Backup, event),
                    )
                    .map_err(map_safe_io)?;
                let persisted_backup = backup_directory
                    .open_file(Path::new(backup_name))
                    .map_err(map_safe_io)?;
                journal.backup_identities[index] =
                    FileIdentity::of(&persisted_backup).map_err(map_safe_io)?;
                self.faults.after(FaultEvent::FilesystemMutation)?;
                journal.actions[index].backed_up = true;
                self.write_journal(directories, &mut journal)?;
            }

            journal.state = JournalState::Promoting { index };
            self.write_journal(directories, &mut journal)?;
            let staged_slot = staging_entry(&staging_state, relative.as_str())?
                .object_slot
                .as_str();
            let staged_file = staged_directory.open_file(Path::new(staged_slot))?;
            project_directory.atomic_write_from_open_recoverable(
                relative_name,
                &staged_file,
                &mut |event| self.faults.atomic_after(AtomicSurface::Project, event),
            )?;
            staged_directory.truncate_open_file(OsStr::new(staged_slot), &staged_file)?;
            self.faults.after(FaultEvent::FilesystemMutation)?;
            journal.actions[index].promoted = true;
            let mut promoted = project_directory.open_file(Path::new(relative.as_str()))?;
            verify_open_md5(&mut promoted, &journal.actions[index].expected_md5)?;
            self.write_journal(directories, &mut journal)?;
        }

        journal.state = JournalState::WritingLocalManifest;
        self.recover_correlated_atomic_publishers(directories, &journal)?;
        self.write_journal(directories, &mut journal)?;
        directories
            .metadata
            .atomic_write(OsStr::new("accepted-manifest.json"), manifest.raw())
            .map_err(map_safe_io)?;
        self.faults.after(FaultEvent::FilesystemMutation)?;
        journal.state = JournalState::Committed;
        self.write_journal(directories, &mut journal)?;
        reclaim_staging_for_terminal(directories, &journal)?;
        control.report(WorkProgress::bounded(
            WorkPhase::Promotion,
            ProgressUnit::Files,
            journal.actions.len().max(1) as u64,
            journal.actions.len().max(1) as u64,
        ));
        if control.is_cancelled() {
            Err(TransactionError::Cancelled)
        } else {
            Ok(PromotionOutcome::Committed)
        }
    }

    #[cfg(test)]
    pub(crate) fn repair(&self) -> Result<RepairOutcome, TransactionError> {
        self.repair_controlled(&OperationControl::default())
    }

    pub(crate) fn repair_controlled(
        &self,
        control: &OperationControl,
    ) -> Result<RepairOutcome, TransactionError> {
        reject_cancelled(control)?;
        let lease = ExclusiveLease::acquire(prepare_layout(self.layout)?)?;
        let directories = &lease.directories;
        let journal_was_recovered =
            !matches!(inspect_pending_journal(directories)?, PendingJournal::Clear);
        if journal_was_recovered {
            directories
                .transactions
                .recover_atomic_for_names(
                    &[(
                        OsStr::new(TRANSACTION_JOURNAL_SLOT),
                        AtomicIntentKind::Publication,
                    )],
                    &mut |event| self.faults.atomic_after(AtomicSurface::Journal, event),
                )
                .map_err(map_safe_io)?;
        }
        // The public journal may have changed identity and phase. Never use a
        // pre-recovery parse for staging correlation or classification.
        let journals = read_journals_from(&directories.transactions)?;
        let staging_had_active_journal = match directories
            .staging
            .open_directory(Path::new(STAGING_CONTAINER))
        {
            Ok(staging) => {
                let state = read_staging_state(&staging)?;
                journals
                    .iter()
                    .any(|journal| staging_is_claimed_by_journal(&state, journal))
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => false,
            Err(error) => return Err(map_safe_io(error)),
        };
        let mut repaired = 0;
        for mut journal in journals {
            let pending_atomic = has_correlated_pending_atomic(directories, &journal)?;
            self.recover_correlated_atomic_publishers(directories, &journal)?;
            if journal.state.is_terminal() {
                if pending_atomic {
                    repaired += 1;
                }
                continue;
            }
            self.reconcile_backup_growth(directories, &mut journal)?;
            self.reconcile_unrecorded_backups(directories, &mut journal)?;
            reject_cancelled(control)?;
            control.report(WorkProgress::bounded(
                WorkPhase::Repair,
                ProgressUnit::Stages,
                repaired as u64,
                1,
            ));
            match classify(directories, &journal)? {
                Recovery::RollForward => {
                    journal.state = JournalState::WritingLocalManifest;
                    self.write_journal(directories, &mut journal)?;
                    let (_, raw) = validated_staged_manifest(directories, &journal)?;
                    directories
                        .metadata
                        .atomic_write(OsStr::new("accepted-manifest.json"), &raw)
                        .map_err(map_safe_io)?;
                    self.faults.after(FaultEvent::FilesystemMutation)?;
                    journal.state = JournalState::Committed;
                    self.write_journal(directories, &mut journal)?;
                    reclaim_staging_for_terminal(directories, &journal)?;
                }
                Recovery::RollBack => self.rollback(directories, &mut journal)?,
                Recovery::Unsafe => return Err(TransactionError::Indeterminate),
            }
            repaired += 1;
            if control.is_cancelled() {
                return Err(TransactionError::Cancelled);
            }
        }
        let staging = match directories
            .staging
            .open_directory(Path::new(STAGING_CONTAINER))
        {
            Ok(staging) => staging,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(if repaired == 0 {
                    RepairOutcome::NothingToRepair
                } else {
                    RepairOutcome::Repaired(repaired)
                });
            }
            Err(error) => return Err(map_safe_io(error)),
        };
        let mut staging_state = read_staging_state(&staging)?;
        if (staging_state.phase.is_download_intent()
            || matches!(staging_state.phase, StagingPhase::Growing { .. }))
            && !staging_had_active_journal
        {
            validate_repairable_staging_intent(&staging, &staging_state)?;
            if staging_state.phase.is_download_intent() {
                reconcile_download_slot_identities(&staging, &mut staging_state)?;
            }
            reconcile_staging_growth(&staging, &staging_state)?;
            staging_state = read_staging_state(&staging)?;
            reclaim_staging(&staging, &staging_state, None)?;
            staging_state.phase = StagingPhase::Inactive;
            staging_state.generation = staging_state
                .generation
                .checked_add(1)
                .ok_or(TransactionError::InvalidJournal)?;
            write_staging_state(&staging, &staging_state)?;
            repaired += 1;
        }
        if journal_was_recovered && repaired == 0 {
            repaired = 1;
        }
        Ok(if repaired == 0 {
            RepairOutcome::NothingToRepair
        } else {
            RepairOutcome::Repaired(repaired)
        })
    }

    /// Remove terminal transaction material after a fresh audit has proved the
    /// promoted installation and transaction surface are both safe. A live
    /// diagnostics export retains the material until its snapshot closes.
    pub(crate) fn prune_terminal(
        &self,
        _evidence: PruneAuditEvidence,
        _cleanup_lease: &CleanupLease,
    ) -> Result<usize, TransactionError> {
        let Some(directories) = open_existing_layout(self.layout)? else {
            return Ok(0);
        };
        let lease = ExclusiveLease::acquire(directories)?;
        let directories = &lease.directories;
        if !matches!(inspect_pending_journal(directories)?, PendingJournal::Clear) {
            directories
                .transactions
                .recover_atomic_for_names(
                    &[(
                        OsStr::new(TRANSACTION_JOURNAL_SLOT),
                        AtomicIntentKind::Publication,
                    )],
                    &mut |event| self.faults.atomic_after(AtomicSurface::Journal, event),
                )
                .map_err(map_safe_io)?;
        }
        // Publisher recovery may have changed both the journal identity and
        // its state. Enumeration is authoritative only after normalization.
        let journals = read_journals_from(&directories.transactions)?;
        if journals.iter().any(|journal| !journal.state.is_terminal()) {
            return Err(TransactionError::Indeterminate);
        }

        let mut removed = 0;
        for journal in journals {
            if matches!(journal.state, JournalState::Pruned) {
                continue;
            }
            self.recover_correlated_atomic_publishers(directories, &journal)?;
            match directories
                .backups
                .open_directory(Path::new(TRANSACTION_BACKUP_SLOT))
            {
                Ok(backup) => {
                    validate_backup_inventory(&backup)?;
                    for action in &journal.actions {
                        let Some(previous) = &action.previous_md5 else {
                            continue;
                        };
                        let slot = action
                            .backup_slot
                            .as_deref()
                            .ok_or(TransactionError::InvalidJournal)?;
                        let mut file = backup.open_file(Path::new(slot)).map_err(map_safe_io)?;
                        if action.backed_up {
                            verify_open_md5(&mut file, previous)?;
                        } else if file.metadata()?.len() != 0 {
                            return Err(TransactionError::Indeterminate);
                        }
                        backup
                            .truncate_open_file(OsStr::new(slot), &file)
                            .map_err(map_safe_io)?;
                    }
                    require_inactive_backup_slots_empty(&backup, &journal)?;
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    if journal
                        .actions
                        .iter()
                        .any(|action| action.previous_md5.is_some())
                    {
                        return Err(TransactionError::Indeterminate);
                    }
                }
                Err(error) => return Err(map_safe_io(error)),
            }
            if journal.manifest_sha256.is_some()
                && let Ok(staging) = directories
                    .staging
                    .open_directory(Path::new(STAGING_CONTAINER))
            {
                let mut state = read_staging_state(&staging)?;
                if matches!(state.phase, StagingPhase::Inactive) {
                    reclaimed_staging_is_clean(&staging)?;
                } else {
                    let (parsed, _) = validated_staged_manifest(directories, &journal)?;
                    require_staging_manifest(&state, &parsed)?;
                    reclaim_staging(&staging, &state, Some(&parsed))?;
                    state.phase = StagingPhase::Inactive;
                    state.generation = state
                        .generation
                        .checked_add(1)
                        .ok_or(TransactionError::InvalidJournal)?;
                    write_staging_state(&staging, &state)?;
                }
            }
            let mut pruned = journal;
            pruned.state = JournalState::Pruned;
            pruned.actions.clear();
            pruned.manifest_sha256 = None;
            self.write_journal(directories, &mut pruned)?;
            removed += 1;
        }
        directories.transactions.sync()?;
        directories.backups.sync()?;
        directories.staging.sync()?;
        Ok(removed)
    }

    fn rollback(
        &self,
        directories: &LayoutDirectories,
        journal: &mut Journal,
    ) -> Result<(), TransactionError> {
        self.recover_correlated_atomic_publishers(directories, journal)?;
        let project_directory = &directories.project;
        let backup_directory = match directories
            .backups
            .open_directory(Path::new(TRANSACTION_BACKUP_SLOT))
        {
            Ok(directory) => {
                validate_backup_inventory(&directory)?;
                require_inactive_backup_slots_empty(&directory, journal)?;
                Some(directory)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(map_safe_io(error)),
        };
        for index in (0..journal.actions.len()).rev() {
            journal.state = JournalState::RollingBack { index };
            self.write_journal(directories, journal)?;
            let action = &journal.actions[index];
            let name = OsStr::new(&action.path);

            match &action.previous_md5 {
                Some(previous) => {
                    if digest_matches_at(project_directory, name, previous)? {
                        continue;
                    }
                    if let Some(mut current) = open_optional_file(project_directory, name)? {
                        verify_open_md5(&mut current, &action.expected_md5)?;
                        project_directory
                            .remove_open_file_bounded(name, &current)
                            .map_err(map_safe_io)?;
                        self.faults.after(FaultEvent::FilesystemMutation)?;
                    }
                    let backup_directory = backup_directory
                        .as_ref()
                        .ok_or(TransactionError::Indeterminate)?;
                    let backup_slot = action
                        .backup_slot
                        .as_deref()
                        .ok_or(TransactionError::InvalidJournal)?;
                    let mut backup = backup_directory
                        .open_file(Path::new(backup_slot))
                        .map_err(map_safe_io)?;
                    verify_open_md5(&mut backup, previous)?;
                    project_directory
                        .atomic_write_from_open_recoverable(name, &backup, &mut |event| {
                            self.faults.atomic_after(AtomicSurface::Project, event)
                        })
                        .map_err(map_safe_io)?;
                    backup_directory
                        .truncate_open_file(OsStr::new(backup_slot), &backup)
                        .map_err(map_safe_io)?;
                    self.faults.after(FaultEvent::FilesystemMutation)?;
                }
                None => {
                    if let Some(mut current) = open_optional_file(project_directory, name)? {
                        verify_open_md5(&mut current, &action.expected_md5)?;
                        project_directory
                            .remove_open_file_bounded(name, &current)
                            .map_err(map_safe_io)?;
                        self.faults.after(FaultEvent::FilesystemMutation)?;
                    }
                    if let (Some(backup), Some(slot)) =
                        (backup_directory.as_ref(), action.backup_slot.as_deref())
                        && let Some(file) = open_optional_file(backup, OsStr::new(slot))?
                        && file.metadata()?.len() != 0
                    {
                        return Err(TransactionError::Indeterminate);
                    }
                }
            }
            self.write_journal(directories, journal)?;
        }
        self.recover_correlated_atomic_publishers(directories, journal)?;
        journal.state = JournalState::RolledBack;
        self.write_journal(directories, journal)?;
        reclaim_staging_for_terminal(directories, journal)?;
        Ok(())
    }

    fn write_journal(
        &self,
        directories: &LayoutDirectories,
        journal: &mut Journal,
    ) -> Result<(), TransactionError> {
        // Each publication of the journal is numbered one past the one it
        // replaces; a new journal is publication 1.
        // The in-memory journal keeps its number until the write succeeded.
        let previous = journal.publication_generation;
        journal.publication_generation = previous
            .checked_add(1)
            .ok_or(TransactionError::InvalidJournal)?;
        let written = serde_json::to_vec(journal)
            .map_err(|_| TransactionError::InvalidJournal)
            .and_then(|bytes| {
                directories
                    .transactions
                    .atomic_write_recoverable(
                        OsStr::new(TRANSACTION_JOURNAL_SLOT),
                        &bytes,
                        &mut |event| self.faults.atomic_after(AtomicSurface::Journal, event),
                    )
                    .map_err(map_safe_io)
            });
        if let Err(error) = written {
            journal.publication_generation = previous;
            return Err(error);
        }
        self.faults.after(FaultEvent::JournalWrite)?;
        Ok(())
    }

    fn reconcile_backup_growth(
        &self,
        directories: &LayoutDirectories,
        journal: &mut Journal,
    ) -> Result<(), TransactionError> {
        let Some(growth) = journal.backup_growth.clone() else {
            return Ok(());
        };
        let backup = directories
            .backups
            .open_directory(Path::new(TRANSACTION_BACKUP_SLOT))
            .map_err(map_safe_io)?;
        let observed = validate_backup_inventory(&backup)?;
        if growth.old_slots != journal.backup_slot_count
            || growth.new_slots < growth.old_slots
            || growth.new_slots > 4096
            || growth.identities.len() < growth.old_slots
            || growth.identities.len() > growth.new_slots
            || journal.backup_identities != growth.identities
            || observed < growth.old_slots
            || observed > growth.new_slots
        {
            return Err(TransactionError::Indeterminate);
        }
        require_all_backup_slots_empty(&backup)?;
        let observed_identities = collect_backup_identities(&backup, observed)?;
        if !observed_identities.starts_with(&growth.identities)
            || !identity_count_is_single_checkpoint_successor(
                growth.identities.len(),
                observed_identities.len(),
            )
        {
            return Err(TransactionError::Indeterminate);
        }
        journal.backup_identities = observed_identities;
        if let Some(current) = journal.backup_growth.as_mut() {
            current.identities = journal.backup_identities.clone();
        }
        self.write_journal(directories, journal)?;
        for index in observed..growth.new_slots {
            let created = backup
                .open_or_create_file(OsStr::new(&backup_object_name(index)))
                .map_err(map_safe_io)?;
            backup.sync().map_err(map_safe_io)?;
            let identity = FileIdentity::of(&created).map_err(map_safe_io)?;
            journal.backup_identities.push(identity);
            if let Some(current) = journal.backup_growth.as_mut() {
                current.identities.push(identity);
            }
            self.write_journal(directories, journal)?;
            self.faults.after(FaultEvent::FilesystemMutation)?;
        }
        backup.sync().map_err(map_safe_io)?;
        self.faults.after(FaultEvent::FilesystemMutation)?;
        journal.backup_slot_count = growth.new_slots;
        journal.backup_growth = None;
        self.write_journal(directories, journal)
    }

    fn reconcile_unrecorded_backups(
        &self,
        directories: &LayoutDirectories,
        journal: &mut Journal,
    ) -> Result<(), TransactionError> {
        let backup = directories
            .backups
            .open_directory(Path::new(TRANSACTION_BACKUP_SLOT))
            .map_err(map_safe_io)?;
        let mut changed = false;
        for (index, action) in journal.actions.iter_mut().enumerate() {
            let (Some(previous), Some(slot)) = (
                action.previous_md5.as_deref(),
                action.backup_slot.as_deref(),
            ) else {
                continue;
            };
            if action.backed_up {
                continue;
            }
            let mut file = backup.open_file(Path::new(slot)).map_err(map_safe_io)?;
            if file.metadata()?.len() == 0 {
                continue;
            }
            verify_open_md5(&mut file, previous)?;
            let observed_identity = FileIdentity::of(&file).map_err(map_safe_io)?;
            if observed_identity != journal.backup_identities[index]
                && !backup
                    .atomic_publication_matches(OsStr::new(slot), &file)
                    .map_err(map_safe_io)?
            {
                return Err(TransactionError::Indeterminate);
            }
            action.backed_up = true;
            journal.backup_identities[index] = observed_identity;
            changed = true;
        }
        if changed {
            self.write_journal(directories, journal)?;
        }
        Ok(())
    }

    fn recover_correlated_atomic_publishers(
        &self,
        directories: &LayoutDirectories,
        journal: &Journal,
    ) -> Result<(), TransactionError> {
        validate_journal(journal)?;
        let project_intents = correlated_project_atomic_intents(journal)?;
        directories
            .project
            .recover_atomic_for_names(&project_intents, &mut |event| {
                self.faults.atomic_after(AtomicSurface::Project, event)
            })
            .map_err(map_safe_io)?;
        match directories
            .backups
            .open_directory(Path::new(TRANSACTION_BACKUP_SLOT))
        {
            Ok(backup) => {
                let backup_intents = correlated_backup_atomic_intents(journal)?;
                backup
                    .recover_atomic_for_names(&backup_intents, &mut |event| {
                        self.faults.atomic_after(AtomicSurface::Backup, event)
                    })
                    .map_err(map_safe_io)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(map_safe_io(error)),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RepairOutcome {
    NothingToRepair,
    Repaired(usize),
}

#[derive(Clone, Debug)]
pub(crate) struct UpdateTransactionProbe {
    layout: UpdateLayout,
}

impl UpdateTransactionProbe {
    pub(crate) fn new(layout: UpdateLayout) -> Self {
        Self { layout }
    }
}

impl TransactionProbe for UpdateTransactionProbe {
    fn probe(&self) -> io::Result<TransactionEvidence> {
        let directories = match open_existing_layout(&self.layout) {
            Ok(Some(directories)) => directories,
            Ok(None) => return Ok(TransactionEvidence::Clear),
            Err(_) => return Ok(TransactionEvidence::Unsafe),
        };
        match inspect_pending_journal(&directories) {
            Ok(PendingJournal::RecoverableResidue) => {
                return Ok(TransactionEvidence::Repairable);
            }
            Ok(PendingJournal::Clear) => {}
            Err(_) => return Ok(TransactionEvidence::Unsafe),
        }
        let journals = match read_journals_from(&directories.transactions) {
            Ok(journals) => journals,
            Err(_) => return Ok(TransactionEvidence::Unsafe),
        };
        let mut has_repairable_staging = false;
        let staging_names = match directories.staging.names() {
            Ok(names) => names,
            Err(_) => return Ok(TransactionEvidence::Unsafe),
        };
        if staging_names
            .iter()
            .any(|name| name != OsStr::new(STAGING_CONTAINER))
        {
            return Ok(TransactionEvidence::Unsafe);
        }
        if !staging_names.is_empty() {
            let staging = match directories
                .staging
                .open_directory(Path::new(STAGING_CONTAINER))
            {
                Ok(staging) => staging,
                Err(_) => return Ok(TransactionEvidence::Unsafe),
            };
            let state = match read_staging_state(&staging) {
                Ok(state) => state,
                Err(_) => return Ok(TransactionEvidence::Unsafe),
            };
            if state.phase.is_download_intent()
                || matches!(state.phase, StagingPhase::Growing { .. })
            {
                let staging_is_claimed = journals
                    .iter()
                    .any(|journal| staging_is_claimed_by_journal(&state, journal));
                if !staging_is_claimed
                    && validate_repairable_staging_intent(&staging, &state).is_ok()
                {
                    has_repairable_staging = true;
                } else if !staging_is_claimed {
                    return Ok(TransactionEvidence::Unsafe);
                }
            } else if reclaimed_staging_is_clean(&staging).is_err() {
                return Ok(TransactionEvidence::Unsafe);
            }
        } else if !journals.is_empty() {
            return Ok(TransactionEvidence::Unsafe);
        }
        let has_active = journals.iter().any(|journal| !journal.state.is_terminal());
        let mut has_pending_atomic = false;
        for journal in &journals {
            let pending_atomic = match has_correlated_pending_atomic(&directories, journal) {
                Ok(pending) => pending,
                Err(_) => return Ok(TransactionEvidence::Unsafe),
            };
            if pending_atomic {
                // Classification assumes normalized public/slot state. Probe
                // is deliberately read-only, so leave the exact correlated
                // publisher intent for Repair under the retained mutation
                // lease instead of classifying an intermediate exchange.
                has_pending_atomic = true;
                continue;
            }
            if journal.state.is_terminal() {
                continue;
            }
            match classify(&directories, journal) {
                Ok(Recovery::RollForward | Recovery::RollBack) => {}
                Ok(Recovery::Unsafe) | Err(_) => return Ok(TransactionEvidence::Unsafe),
            }
        }
        if has_active || has_repairable_staging || has_pending_atomic {
            Ok(TransactionEvidence::Repairable)
        } else {
            Ok(TransactionEvidence::Clear)
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct Journal {
    schema: u32,
    #[serde(default)]
    publication_generation: u64,
    transaction_id: String,
    manifest_id: String,
    #[serde(default)]
    manifest_sha256: Option<String>,
    state: JournalState,
    #[serde(default)]
    backup_slot_count: usize,
    #[serde(default)]
    backup_identities: Vec<FileIdentity>,
    #[serde(default)]
    backup_growth: Option<BackupGrowth>,
    actions: Vec<ActionRecord>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct BackupGrowth {
    old_slots: usize,
    new_slots: usize,
    identities: Vec<FileIdentity>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct ActionRecord {
    path: String,
    expected_md5: String,
    previous_md5: Option<String>,
    #[serde(default)]
    backup_slot: Option<String>,
    backed_up: bool,
    promoted: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum JournalState {
    Planned,
    BackingUp { index: usize },
    Promoting { index: usize },
    WritingLocalManifest,
    Committed,
    RollingBack { index: usize },
    RolledBack,
    Pruned,
}

impl JournalState {
    fn is_terminal(&self) -> bool {
        matches!(self, Self::Committed | Self::RolledBack | Self::Pruned)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Recovery {
    RollForward,
    RollBack,
    Unsafe,
}

#[derive(Debug)]
pub(crate) enum TransactionError {
    Io(io::Error),
    Busy,
    UnsafePath,
    InvalidIdentifier,
    InvalidJournal,
    DigestMismatch,
    Indeterminate,
    Cancelled,
}

impl fmt::Display for TransactionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "update transaction I/O failed: {error}"),
            other => write!(formatter, "update transaction failed: {other:?}"),
        }
    }
}

impl Error for TransactionError {}

impl From<io::Error> for TransactionError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

fn classify(
    directories: &LayoutDirectories,
    journal: &Journal,
) -> Result<Recovery, TransactionError> {
    validate_journal(journal)?;
    let backup_directory = match directories
        .backups
        .open_directory(Path::new(TRANSACTION_BACKUP_SLOT))
    {
        Ok(directory) => {
            validate_backup_inventory(&directory)?;
            Some(directory)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(map_safe_io(error)),
    };
    if let Some(backup) = backup_directory.as_ref() {
        if let Some(growth) = &journal.backup_growth {
            let observed = validate_backup_inventory(backup)?;
            if observed < growth.old_slots || observed > growth.new_slots {
                return Ok(Recovery::Unsafe);
            }
            let observed_identities = collect_backup_identities(backup, observed)?;
            if !observed_identities.starts_with(&growth.identities) {
                return Ok(Recovery::Unsafe);
            }
            require_all_backup_slots_empty(backup)?;
        } else {
            require_inactive_backup_slots_empty(backup, journal)?;
        }
    }
    let all_new = journal.actions.iter().try_fold(true, |all, action| {
        Ok::<_, TransactionError>(
            all && digest_matches_at(
                &directories.project,
                OsStr::new(&action.path),
                &action.expected_md5,
            )?,
        )
    })?;
    if all_new {
        return Ok(if validated_staged_manifest(directories, journal).is_ok() {
            Recovery::RollForward
        } else {
            Recovery::Unsafe
        });
    }

    for action in &journal.actions {
        let name = OsStr::new(&action.path);
        let safe = match &action.previous_md5 {
            Some(previous) => {
                let final_is_previous = digest_matches_at(&directories.project, name, previous)?;
                let backup_is_previous = match backup_directory.as_ref() {
                    Some(backup) => match action.backup_slot.as_deref() {
                        Some(slot) => digest_matches_at(backup, OsStr::new(slot), previous)?,
                        None => false,
                    },
                    None => false,
                };
                final_is_previous
                    || (backup_is_previous
                        && (open_optional_file(&directories.project, name)?.is_none()
                            || digest_matches_at(
                                &directories.project,
                                name,
                                &action.expected_md5,
                            )?))
            }
            None => {
                (open_optional_file(&directories.project, name)?.is_none()
                    || digest_matches_at(&directories.project, name, &action.expected_md5)?)
                    && match backup_directory.as_ref() {
                        Some(backup) => match action.backup_slot.as_deref() {
                            Some(slot) => {
                                open_optional_file(backup, OsStr::new(slot))?.is_none_or(|file| {
                                    file.metadata().is_ok_and(|meta| meta.len() == 0)
                                })
                            }
                            None => true,
                        },
                        None => true,
                    }
            }
        };
        if !safe {
            return Ok(Recovery::Unsafe);
        }
    }
    Ok(Recovery::RollBack)
}

fn has_correlated_pending_atomic(
    directories: &LayoutDirectories,
    journal: &Journal,
) -> Result<bool, TransactionError> {
    validate_journal(journal)?;
    let project_intents = correlated_project_atomic_intents(journal)?;
    let project_pending = directories
        .project
        .has_pending_atomic_for_names(&project_intents)
        .map_err(map_safe_io)?;
    let backup_pending = match directories
        .backups
        .open_directory(Path::new(TRANSACTION_BACKUP_SLOT))
    {
        Ok(backup) => {
            let backup_intents = correlated_backup_atomic_intents(journal)?;
            backup
                .has_pending_atomic_for_names(&backup_intents)
                .map_err(map_safe_io)?
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        Err(error) => return Err(map_safe_io(error)),
    };
    Ok(project_pending || backup_pending)
}

#[derive(Debug)]
enum PendingJournal {
    Clear,
    RecoverableResidue,
}

fn inspect_pending_journal(
    directories: &LayoutDirectories,
) -> Result<PendingJournal, TransactionError> {
    // A journal write is a rename: it happened or not. What an interrupted
    // one leaves is residue for Repair to remove.
    Ok(
        match directories
            .transactions
            .pending_atomic_intent(MAX_JOURNAL_BYTES)
            .map_err(map_safe_io)?
        {
            AtomicIntentSnapshot::Clear => PendingJournal::Clear,
            AtomicIntentSnapshot::RecoverableResidue => PendingJournal::RecoverableResidue,
        },
    )
}

/// Decodes one journal and rebinds any schema-2 `{device, inode}` backup
/// identities onto `filesystem`, the transactions directory's filesystem id.
/// Journals are Launcher-published payloads that are only rewritten through
/// the transaction protocol, so a legacy journal is upgraded in memory on
/// every read and reaches the current encoding with its next publication.
/// The rebound identities are then compared strictly, see
/// [`FileIdentity::migrate_legacy`].
fn parse_journal_bytes(bytes: &[u8], filesystem: u64) -> Result<Journal, TransactionError> {
    if bytes.len() as u64 > MAX_JOURNAL_BYTES {
        return Err(TransactionError::InvalidJournal);
    }
    let mut journal: Journal =
        serde_json::from_slice(bytes).map_err(|_| TransactionError::InvalidJournal)?;
    validate_journal(&journal)?;
    let growth = journal
        .backup_growth
        .as_mut()
        .map(|growth| growth.identities.iter_mut());
    FileIdentity::migrate_legacy(
        journal
            .backup_identities
            .iter_mut()
            .chain(growth.into_iter().flatten()),
        filesystem,
    )
    .map_err(|_| TransactionError::InvalidJournal)?;
    Ok(journal)
}

fn action_is_untouched(action: &ActionRecord) -> bool {
    !action.backed_up && !action.promoted
}

fn action_backup_complete(action: &ActionRecord) -> bool {
    action.previous_md5.is_none() || action.backed_up
}

fn action_is_complete(action: &ActionRecord) -> bool {
    action_backup_complete(action) && action.promoted
}

fn identity_count_is_single_checkpoint_successor(previous: usize, intended: usize) -> bool {
    intended == previous || previous.checked_add(1) == Some(intended)
}

fn correlated_project_atomic_intents(
    journal: &Journal,
) -> Result<Vec<(&OsStr, AtomicIntentKind)>, TransactionError> {
    let (index, rolling_back) = match &journal.state {
        JournalState::Promoting { index } => (*index, false),
        JournalState::RollingBack { index } => (*index, true),
        _ => return Ok(Vec::new()),
    };
    let action = journal
        .actions
        .get(index)
        .ok_or(TransactionError::InvalidJournal)?;
    let name = OsStr::new(&action.path);
    if rolling_back {
        let mut intents = vec![(name, AtomicIntentKind::Removal)];
        if action.previous_md5.is_some() {
            intents.push((name, AtomicIntentKind::Publication));
        }
        Ok(intents)
    } else {
        Ok(vec![(name, AtomicIntentKind::Publication)])
    }
}

fn correlated_backup_atomic_intents(
    journal: &Journal,
) -> Result<Vec<(&OsStr, AtomicIntentKind)>, TransactionError> {
    let JournalState::BackingUp { index } = &journal.state else {
        return Ok(Vec::new());
    };
    let action = journal
        .actions
        .get(*index)
        .ok_or(TransactionError::InvalidJournal)?;
    if action.previous_md5.is_none() {
        return Ok(Vec::new());
    }
    let slot = action
        .backup_slot
        .as_deref()
        .ok_or(TransactionError::InvalidJournal)?;
    Ok(vec![(OsStr::new(slot), AtomicIntentKind::Publication)])
}

fn validated_staged_manifest(
    directories: &LayoutDirectories,
    journal: &Journal,
) -> Result<(Pd2Manifest, Vec<u8>), TransactionError> {
    let staging = directories
        .staging
        .open_directory(Path::new(STAGING_CONTAINER))
        .map_err(map_safe_io)?;
    let state = read_staging_state(&staging)?;
    let raw = read_bounded_open(
        staging
            .open_file(Path::new(STAGING_MANIFEST))
            .map_err(map_safe_io)?,
        1024 * 1024,
    )?;
    let observed_sha = hex(&Sha256::digest(&raw));
    if journal.manifest_sha256.as_deref() != Some(observed_sha.as_str())
        || state.manifest_sha256 != observed_sha
        || state.manifest_id != journal.manifest_id
        || !state.phase.is_download_intent()
    {
        return Err(TransactionError::Indeterminate);
    }
    match &state.phase {
        StagingPhase::Downloading { transaction_id }
        | StagingPhase::ReadyForJournal { transaction_id }
            if transaction_id != &journal.transaction_id =>
        {
            return Err(TransactionError::Indeterminate);
        }
        _ => {}
    }
    let manifest = Pd2Manifest::parse(ManifestResponse {
        requested_url: PD2_MANIFEST_URL.to_owned(),
        final_url: PD2_MANIFEST_URL.to_owned(),
        declared_length: Some(raw.len() as u64),
        body: raw.clone(),
    })
    .map_err(|_| TransactionError::InvalidJournal)?;
    if manifest.id() != journal.manifest_id {
        return Err(TransactionError::Indeterminate);
    }
    require_staging_manifest(&state, &manifest)?;
    for action in &journal.actions {
        let Some(entry) = manifest
            .entries()
            .iter()
            .find(|entry| entry.path().as_str() == action.path)
        else {
            return Err(TransactionError::Indeterminate);
        };
        if hex(&entry.digest().bytes()) != action.expected_md5 {
            return Err(TransactionError::Indeterminate);
        }
    }
    Ok((manifest, raw))
}

fn reclaim_staging_for_terminal(
    directories: &LayoutDirectories,
    journal: &Journal,
) -> Result<(), TransactionError> {
    if journal.manifest_sha256.is_none() {
        return Ok(());
    }
    let staging = directories
        .staging
        .open_directory(Path::new(STAGING_CONTAINER))
        .map_err(map_safe_io)?;
    let mut state = read_staging_state(&staging)?;
    if matches!(state.phase, StagingPhase::Inactive) {
        return reclaimed_staging_is_clean(&staging);
    }
    let (manifest, _) = validated_staged_manifest(directories, journal)?;
    reclaim_staging(&staging, &state, Some(&manifest))?;
    state.phase = StagingPhase::Inactive;
    state.generation = state
        .generation
        .checked_add(1)
        .ok_or(TransactionError::InvalidJournal)?;
    write_staging_state(&staging, &state)
}

fn validate_repairable_staging_intent(
    staging: &Directory,
    state: &StagingState,
) -> Result<(), TransactionError> {
    validate_staging_state(state)?;
    let (files, partials) = observe_staging_inventory(staging)?;
    match state.phase {
        StagingPhase::Growing {
            old_slots,
            new_slots,
        } => {
            if files < old_slots
                || partials < old_slots
                || files > new_slots
                || partials > new_slots
                || files.abs_diff(partials) > 1
            {
                return Err(TransactionError::Indeterminate);
            }
            for directory_name in ["files", ".partial"] {
                let directory = staging
                    .open_directory(Path::new(directory_name))
                    .map_err(map_safe_io)?;
                require_slots_empty(&directory)?;
            }
        }
        StagingPhase::Downloading { .. } | StagingPhase::ReadyForJournal { .. } => {
            if files != state.slot_count || partials != state.slot_count {
                return Err(TransactionError::Indeterminate);
            }
            let raw = read_bounded_open(
                staging
                    .open_file(Path::new(STAGING_MANIFEST))
                    .map_err(map_safe_io)?,
                1024 * 1024,
            )?;
            let manifest = Pd2Manifest::parse(ManifestResponse {
                requested_url: PD2_MANIFEST_URL.to_owned(),
                final_url: PD2_MANIFEST_URL.to_owned(),
                declared_length: Some(raw.len() as u64),
                body: raw,
            })
            .map_err(|_| TransactionError::InvalidJournal)?;
            require_staging_manifest(state, &manifest)?;
        }
        StagingPhase::Inactive => return Err(TransactionError::Indeterminate),
    }
    Ok(())
}

fn validate_journal(journal: &Journal) -> Result<(), TransactionError> {
    if journal.schema != JOURNAL_SCHEMA || journal.publication_generation == 0 {
        return Err(TransactionError::InvalidJournal);
    }
    if journal.publication_generation == 1 && !matches!(journal.state, JournalState::Planned) {
        return Err(TransactionError::InvalidJournal);
    }
    validate_identifier(&journal.transaction_id)?;
    validate_identifier(&journal.manifest_id)?;
    if journal.manifest_sha256.as_ref().is_some_and(|digest| {
        digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit())
    }) {
        return Err(TransactionError::InvalidJournal);
    }
    let effective_slots = journal
        .backup_growth
        .as_ref()
        .map_or(journal.backup_slot_count, |growth| growth.new_slots);
    if effective_slots < journal.actions.len() || effective_slots > 4096 {
        return Err(TransactionError::InvalidJournal);
    }
    if let Some(growth) = &journal.backup_growth
        && (growth.old_slots != journal.backup_slot_count
            || growth.old_slots >= growth.new_slots
            || growth.identities.len() < growth.old_slots
            || growth.identities.len() > growth.new_slots
            || journal.backup_identities != growth.identities)
    {
        return Err(TransactionError::InvalidJournal);
    }
    if journal.backup_growth.is_none()
        && journal.backup_identities.len() != journal.backup_slot_count
    {
        return Err(TransactionError::InvalidJournal);
    }
    for (index, action) in journal.actions.iter().enumerate() {
        RelativeGamePath::parse(&action.path).map_err(|_| TransactionError::InvalidJournal)?;
        parse_md5_hex(&action.expected_md5)?;
        if let Some(previous) = &action.previous_md5 {
            parse_md5_hex(previous)?;
        }
        let Some(slot) = action.backup_slot.as_deref() else {
            return Err(TransactionError::InvalidJournal);
        };
        if slot != backup_object_name(index) {
            return Err(TransactionError::InvalidJournal);
        }
        if action.previous_md5.is_none() && action.backed_up {
            return Err(TransactionError::InvalidJournal);
        }
        if action.promoted && !action_backup_complete(action) {
            return Err(TransactionError::InvalidJournal);
        }
    }
    validate_journal_phase_shape(journal)
}

fn validate_journal_phase_shape(journal: &Journal) -> Result<(), TransactionError> {
    let prefix_is_valid = || {
        let mut incomplete_seen = false;
        for action in &journal.actions {
            if action_is_complete(action) {
                if incomplete_seen {
                    return false;
                }
            } else {
                incomplete_seen = true;
            }
        }
        true
    };
    let indexed_shape = |index: usize, promoting: bool| {
        if index >= journal.actions.len() {
            return false;
        }
        journal.actions[..index].iter().all(action_is_complete)
            && journal.actions[index + 1..].iter().all(action_is_untouched)
            && if promoting {
                action_backup_complete(&journal.actions[index])
            } else {
                !journal.actions[index].promoted
            }
    };

    let legal = match &journal.state {
        JournalState::Planned => {
            journal.actions.iter().all(action_is_untouched) && journal.manifest_sha256.is_some()
        }
        JournalState::BackingUp { index } => {
            journal.backup_growth.is_none()
                && journal.manifest_sha256.is_some()
                && indexed_shape(*index, false)
        }
        JournalState::Promoting { index } => {
            journal.backup_growth.is_none()
                && journal.manifest_sha256.is_some()
                && indexed_shape(*index, true)
        }
        JournalState::WritingLocalManifest | JournalState::Committed => {
            journal.backup_growth.is_none()
                && journal.manifest_sha256.is_some()
                && prefix_is_valid()
        }
        JournalState::RollingBack { index } => {
            journal.backup_growth.is_none()
                && journal.manifest_sha256.is_some()
                && *index < journal.actions.len()
                && prefix_is_valid()
        }
        JournalState::RolledBack => {
            journal.backup_growth.is_none()
                && journal.manifest_sha256.is_some()
                && prefix_is_valid()
        }
        JournalState::Pruned => {
            journal.backup_growth.is_none()
                && journal.manifest_sha256.is_none()
                && journal.actions.is_empty()
        }
    };
    if legal {
        Ok(())
    } else {
        Err(TransactionError::InvalidJournal)
    }
}

fn read_journals_from(transactions: &Directory) -> Result<Vec<Journal>, TransactionError> {
    let filesystem = transactions.filesystem_id().map_err(map_safe_io)?;
    let mut names = transactions.names().map_err(map_safe_io)?;
    names.sort();
    names
        .into_iter()
        .map(|name| {
            if Path::new(&name)
                .extension()
                .and_then(|value| value.to_str())
                != Some("json")
            {
                return Err(TransactionError::InvalidJournal);
            }
            let bytes = read_bounded_open(
                transactions
                    .open_file(Path::new(&name))
                    .map_err(map_safe_io)?,
                MAX_JOURNAL_BYTES,
            )?;
            parse_journal_bytes(&bytes, filesystem)
        })
        .collect()
}

fn prepare_staging_container(
    staging_root: &Directory,
    transaction_id: &str,
    manifest: &Pd2Manifest,
) -> Result<(), TransactionError> {
    let staging = staging_root
        .ensure_directory(Path::new(STAGING_CONTAINER))
        .map_err(map_safe_io)?;
    let files = staging
        .ensure_directory(Path::new("files"))
        .map_err(map_safe_io)?;
    let partial = staging
        .ensure_directory(Path::new(".partial"))
        .map_err(map_safe_io)?;
    let (observed_files, observed_partials) = observe_staging_inventory(&staging)?;
    let current = read_staging_state_optional(&staging)?;
    if let Some(state) = current.as_ref() {
        reconcile_staging_growth(&staging, state)?;
    } else if observed_files != 0 || observed_partials != 0 {
        return Err(TransactionError::Indeterminate);
    }
    let observed_slots = validate_staging_inventory(&staging)?;
    if let Some(state) = current.as_ref()
        && state.phase.is_download_intent()
    {
        require_staging_manifest(state, manifest)?;
        let observed = read_bounded_open(
            staging
                .open_file(Path::new(STAGING_MANIFEST))
                .map_err(map_safe_io)?,
            1024 * 1024,
        )?;
        if observed != manifest.raw() {
            return Err(TransactionError::Indeterminate);
        }
        let mut resumed = state.clone();
        if resumed.phase.transaction_id() != Some(transaction_id) {
            if !matches!(resumed.phase, StagingPhase::Downloading { .. }) {
                return Err(TransactionError::Indeterminate);
            }
            resumed.generation = resumed
                .generation
                .checked_add(1)
                .ok_or(TransactionError::InvalidJournal)?;
            resumed.phase = StagingPhase::Downloading {
                transaction_id: transaction_id.to_owned(),
            };
        }
        write_staging_state(&staging, &resumed)?;
        return Ok(());
    }
    require_slots_empty(&files)?;
    require_slots_empty(&partial)?;

    let entries = manifest
        .entries()
        .iter()
        .enumerate()
        .map(|(index, entry)| StagingEntry {
            path: entry.path().as_str().to_owned(),
            md5: hex(&entry.digest().bytes()),
            object_slot: numbered_name(STAGING_OBJECT_PREFIX, index),
            partial_slot: numbered_name(STAGING_PARTIAL_PREFIX, index),
        })
        .collect::<Vec<_>>();
    let slot_count = current
        .as_ref()
        .map_or(observed_slots, |state| state.slot_count)
        .max(entries.len());
    let generation = current
        .as_ref()
        .map_or(0, |state| state.generation)
        .checked_add(1)
        .ok_or(TransactionError::InvalidJournal)?;
    let mut state = StagingState {
        schema: STAGING_STATE_SCHEMA,
        generation,
        manifest_id: manifest.id().to_owned(),
        manifest_sha256: hex(&Sha256::digest(manifest.raw())),
        phase: StagingPhase::Growing {
            old_slots: observed_slots,
            new_slots: slot_count,
        },
        slot_count,
        slot_identities: match current.as_ref() {
            // Proven empty above: rebound, as a copied container needs.
            Some(state)
                if state.phase == StagingPhase::Inactive && copied_idle_slots_accepted() =>
            {
                collect_staging_identities(&staging, state.slot_count)?
            }
            Some(state) => state.slot_identities.clone(),
            None => Vec::new(),
        },
        entries,
    };
    // The recoverable growth intent is durable before the first new directory
    // entry. A restart may now reconcile only contiguous, empty additions.
    write_staging_state(&staging, &state)?;
    for index in observed_slots..slot_count {
        let object = files
            .open_or_create_file(OsStr::new(&numbered_name(STAGING_OBJECT_PREFIX, index)))
            .map_err(map_safe_io)?;
        let partial_file = partial
            .open_or_create_file(OsStr::new(&numbered_name(STAGING_PARTIAL_PREFIX, index)))
            .map_err(map_safe_io)?;
        files.sync().map_err(map_safe_io)?;
        partial.sync().map_err(map_safe_io)?;
        state.slot_identities.push(StagingSlotIdentity {
            object: FileIdentity::of(&object).map_err(map_safe_io)?,
            partial: FileIdentity::of(&partial_file).map_err(map_safe_io)?,
        });
        state.generation = state
            .generation
            .checked_add(1)
            .ok_or(TransactionError::InvalidJournal)?;
        write_staging_state(&staging, &state)?;
    }
    files.sync().map_err(map_safe_io)?;
    partial.sync().map_err(map_safe_io)?;
    staging
        .atomic_write(OsStr::new(STAGING_MANIFEST), manifest.raw())
        .map_err(map_safe_io)?;
    state.generation = state
        .generation
        .checked_add(1)
        .ok_or(TransactionError::InvalidJournal)?;
    state.phase = StagingPhase::Downloading {
        transaction_id: transaction_id.to_owned(),
    };
    write_staging_state(&staging, &state)
}

fn write_staging_state(staging: &Directory, state: &StagingState) -> Result<(), TransactionError> {
    validate_staging_state(state)?;
    let payload = serde_json::to_vec(state).map_err(|_| TransactionError::InvalidJournal)?;
    let bytes = serde_json::to_vec(&StagingEnvelope {
        state: state.clone(),
        checksum: hex(&Sha256::digest(&payload)),
    })
    .map_err(|_| TransactionError::InvalidJournal)?;
    if bytes.len() as u64 > STAGING_MAP_LIMIT {
        return Err(TransactionError::InvalidJournal);
    }
    staging
        .atomic_write(OsStr::new(STAGING_MAP_A), &bytes)
        .map_err(map_safe_io)?;
    staging
        .atomic_write(OsStr::new(STAGING_MAP_B), &bytes)
        .map_err(map_safe_io)
}

fn read_staging_state(staging: &Directory) -> Result<StagingState, TransactionError> {
    read_staging_state_optional(staging)?.ok_or(TransactionError::InvalidJournal)
}

fn read_staging_state_optional(
    staging: &Directory,
) -> Result<Option<StagingState>, TransactionError> {
    let mut present = 0_usize;
    let mut valid = Vec::new();
    let filesystem = staging.filesystem_id().map_err(map_safe_io)?;
    for name in [STAGING_MAP_A, STAGING_MAP_B] {
        match staging.open_file(Path::new(name)) {
            Ok(file) => {
                present += 1;
                if let Ok(mut state) = read_staging_map(file) {
                    // Schema-2 slot identities are rebound onto the staging
                    // directory's filesystem after the checksum over their
                    // legacy encoding was verified, then compared strictly
                    // (see `FileIdentity::migrate_legacy`). The next staging
                    // write persists the current encoding to both maps.
                    let identities = state
                        .slot_identities
                        .iter_mut()
                        .flat_map(|slot| [&mut slot.object, &mut slot.partial]);
                    if FileIdentity::migrate_legacy(identities, filesystem).is_ok() {
                        valid.push(state);
                    }
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(map_safe_io(error)),
        }
    }
    if valid.is_empty() {
        return if present != 0 {
            Err(TransactionError::InvalidJournal)
        } else {
            Ok(None)
        };
    }
    let generation = valid
        .iter()
        .map(|state| state.generation)
        .max()
        .ok_or(TransactionError::InvalidJournal)?;
    let current = valid
        .iter()
        .filter(|state| state.generation == generation)
        .collect::<Vec<_>>();
    if current.windows(2).any(|pair| pair[0] != pair[1]) {
        return Err(TransactionError::InvalidJournal);
    }
    Ok(current.first().map(|state| (*state).clone()))
}

fn read_staging_map(file: File) -> Result<StagingState, TransactionError> {
    let bytes = read_bounded_open(file, STAGING_MAP_LIMIT)?;
    let envelope: StagingEnvelope =
        serde_json::from_slice(&bytes).map_err(|_| TransactionError::InvalidJournal)?;
    let payload =
        serde_json::to_vec(&envelope.state).map_err(|_| TransactionError::InvalidJournal)?;
    if envelope.checksum != hex(&Sha256::digest(&payload)) {
        return Err(TransactionError::InvalidJournal);
    }
    validate_staging_state(&envelope.state)?;
    Ok(envelope.state)
}

fn validate_staging_state(state: &StagingState) -> Result<(), TransactionError> {
    if state.schema != STAGING_STATE_SCHEMA
        || validate_identifier(&state.manifest_id).is_err()
        || state.manifest_sha256.len() != 64
        || !state
            .manifest_sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        || state.entries.is_empty()
        || state.entries.len() > 4096
        || state.slot_count < state.entries.len()
        || state.slot_count > 4096
    {
        return Err(TransactionError::InvalidJournal);
    }
    let identity_count_valid = match state.phase {
        StagingPhase::Growing {
            old_slots,
            new_slots,
        } => state.slot_identities.len() >= old_slots && state.slot_identities.len() <= new_slots,
        _ => state.slot_identities.len() == state.slot_count,
    };
    if !identity_count_valid {
        return Err(TransactionError::InvalidJournal);
    }
    match &state.phase {
        StagingPhase::Inactive => {}
        StagingPhase::Growing {
            old_slots,
            new_slots,
        } if old_slots <= new_slots && *new_slots == state.slot_count => {}
        StagingPhase::Downloading { transaction_id }
        | StagingPhase::ReadyForJournal { transaction_id }
            if validate_identifier(transaction_id).is_ok() => {}
        _ => return Err(TransactionError::InvalidJournal),
    }
    for (index, entry) in state.entries.iter().enumerate() {
        RelativeGamePath::parse(&entry.path).map_err(|_| TransactionError::InvalidJournal)?;
        parse_md5_hex(&entry.md5)?;
        if entry.object_slot != numbered_name(STAGING_OBJECT_PREFIX, index)
            || entry.partial_slot != numbered_name(STAGING_PARTIAL_PREFIX, index)
        {
            return Err(TransactionError::InvalidJournal);
        }
    }
    Ok(())
}

fn require_staging_manifest(
    state: &StagingState,
    manifest: &Pd2Manifest,
) -> Result<(), TransactionError> {
    if !state.phase.is_download_intent()
        || state.manifest_id != manifest.id()
        || state.manifest_sha256 != hex(&Sha256::digest(manifest.raw()))
        || state.entries.len() != manifest.entries().len()
    {
        return Err(TransactionError::Indeterminate);
    }
    for (state_entry, manifest_entry) in state.entries.iter().zip(manifest.entries()) {
        if state_entry.path != manifest_entry.path().as_str()
            || state_entry.md5 != hex(&manifest_entry.digest().bytes())
        {
            return Err(TransactionError::Indeterminate);
        }
    }
    Ok(())
}

fn staging_entry<'a>(
    state: &'a StagingState,
    path: &str,
) -> Result<&'a StagingEntry, TransactionError> {
    state
        .entries
        .iter()
        .find(|entry| entry.path == path)
        .ok_or(TransactionError::InvalidJournal)
}

fn validate_staging_inventory(staging: &Directory) -> Result<usize, TransactionError> {
    let (files, partials) = observe_staging_inventory(staging)?;
    if files != partials {
        return Err(TransactionError::Indeterminate);
    }
    Ok(files)
}

fn observe_staging_inventory(staging: &Directory) -> Result<(usize, usize), TransactionError> {
    let allowed = [
        "files",
        ".partial",
        STAGING_MANIFEST,
        STAGING_MAP_A,
        STAGING_MAP_B,
    ];
    if staging
        .names()
        .map_err(map_safe_io)?
        .iter()
        .any(|name| !allowed.iter().any(|allowed| name == OsStr::new(allowed)))
    {
        return Err(TransactionError::Indeterminate);
    }
    let mut counts = Vec::new();
    for (directory_name, prefix) in [
        ("files", STAGING_OBJECT_PREFIX),
        (".partial", STAGING_PARTIAL_PREFIX),
    ] {
        let directory = staging
            .open_directory(Path::new(directory_name))
            .map_err(map_safe_io)?;
        let mut indices = Vec::new();
        for name in directory.names().map_err(map_safe_io)? {
            let Some(name) = name.to_str() else {
                return Err(TransactionError::Indeterminate);
            };
            if !is_numbered_name(name, prefix) {
                return Err(TransactionError::Indeterminate);
            }
            directory.open_file(Path::new(name)).map_err(map_safe_io)?;
            indices.push(
                name.strip_prefix(prefix)
                    .and_then(|index| index.parse::<usize>().ok())
                    .ok_or(TransactionError::Indeterminate)?,
            );
        }
        indices.sort_unstable();
        if indices.iter().copied().ne(0..indices.len()) {
            return Err(TransactionError::Indeterminate);
        }
        counts.push(indices.len());
    }
    Ok((counts[0], counts[1]))
}

fn reconcile_staging_growth(
    staging: &Directory,
    state: &StagingState,
) -> Result<(), TransactionError> {
    let StagingPhase::Growing {
        old_slots,
        new_slots,
    } = state.phase
    else {
        if state.phase == StagingPhase::Inactive && copied_idle_slots_accepted() {
            return idle_staging_is_empty(staging, state);
        }
        let observed = validate_staging_inventory(staging)?;
        return if observed == state.slot_count
            && staging_identities_match(staging, &state.slot_identities)?
        {
            Ok(())
        } else {
            Err(TransactionError::Indeterminate)
        };
    };
    if old_slots > new_slots || new_slots != state.slot_count || new_slots > 4096 {
        return Err(TransactionError::InvalidJournal);
    }
    let files = staging
        .open_directory(Path::new("files"))
        .map_err(map_safe_io)?;
    let partials = staging
        .open_directory(Path::new(".partial"))
        .map_err(map_safe_io)?;
    let (file_count, partial_count) = observe_staging_inventory(staging)?;
    if file_count < old_slots
        || partial_count < old_slots
        || file_count > new_slots
        || partial_count > new_slots
        || file_count.abs_diff(partial_count) > 1
    {
        return Err(TransactionError::Indeterminate);
    }
    require_slots_empty(&files)?;
    require_slots_empty(&partials)?;
    if !staging_identities_match(staging, &state.slot_identities)? {
        return Err(TransactionError::Indeterminate);
    }
    for index in file_count..new_slots {
        files
            .open_or_create_file(OsStr::new(&numbered_name(STAGING_OBJECT_PREFIX, index)))
            .map_err(map_safe_io)?;
    }
    for index in partial_count..new_slots {
        partials
            .open_or_create_file(OsStr::new(&numbered_name(STAGING_PARTIAL_PREFIX, index)))
            .map_err(map_safe_io)?;
    }
    files.sync().map_err(map_safe_io)?;
    partials.sync().map_err(map_safe_io)?;
    let identities = collect_staging_identities(staging, new_slots)?;
    let mut reconciled = state.clone();
    reconciled.generation = reconciled
        .generation
        .checked_add(1)
        .ok_or(TransactionError::InvalidJournal)?;
    reconciled.slot_identities = identities;
    write_staging_state(staging, &reconciled)
}

fn collect_staging_identities(
    staging: &Directory,
    count: usize,
) -> Result<Vec<StagingSlotIdentity>, TransactionError> {
    let files = staging
        .open_directory(Path::new("files"))
        .map_err(map_safe_io)?;
    let partials = staging
        .open_directory(Path::new(".partial"))
        .map_err(map_safe_io)?;
    (0..count)
        .map(|index| {
            let object = files
                .open_file(Path::new(&numbered_name(STAGING_OBJECT_PREFIX, index)))
                .map_err(map_safe_io)?;
            let partial = partials
                .open_file(Path::new(&numbered_name(STAGING_PARTIAL_PREFIX, index)))
                .map_err(map_safe_io)?;
            Ok(StagingSlotIdentity {
                object: FileIdentity::of(&object).map_err(map_safe_io)?,
                partial: FileIdentity::of(&partial).map_err(map_safe_io)?,
            })
        })
        .collect()
}

fn staging_identities_match(
    staging: &Directory,
    expected: &[StagingSlotIdentity],
) -> Result<bool, TransactionError> {
    let observed = collect_staging_identities(staging, expected.len())?;
    Ok(observed == expected)
}

fn reconcile_download_slot_identities(
    staging: &Directory,
    state: &mut StagingState,
) -> Result<(), TransactionError> {
    let observed = collect_staging_identities(staging, state.slot_count)?;
    if observed == state.slot_identities {
        return Ok(());
    }
    let files = staging
        .open_directory(Path::new("files"))
        .map_err(map_safe_io)?;
    let partials = staging
        .open_directory(Path::new(".partial"))
        .map_err(map_safe_io)?;
    for (index, (expected, actual)) in state.slot_identities.iter_mut().zip(observed).enumerate() {
        if expected.partial != actual.partial {
            return Err(TransactionError::Indeterminate);
        }
        if expected.object != actual.object {
            let mut object = files
                .open_file(Path::new(&state.entries[index].object_slot))
                .map_err(map_safe_io)?;
            verify_open_md5(&mut object, &state.entries[index].md5)?;
            *expected = actual;
        }
        let partial = partials
            .open_file(Path::new(&state.entries[index].partial_slot))
            .map_err(map_safe_io)?;
        if !actual.partial.matches(&partial).map_err(map_safe_io)? {
            return Err(TransactionError::Indeterminate);
        }
    }
    state.generation = state
        .generation
        .checked_add(1)
        .ok_or(TransactionError::InvalidJournal)?;
    write_staging_state(staging, state)
}

fn require_slots_empty(directory: &Directory) -> Result<(), TransactionError> {
    for name in directory.names().map_err(map_safe_io)? {
        let file = directory.open_file(Path::new(&name)).map_err(map_safe_io)?;
        if file.metadata()?.len() != 0 {
            return Err(TransactionError::Indeterminate);
        }
    }
    Ok(())
}

fn reclaim_staging(
    staging: &Directory,
    state: &StagingState,
    manifest: Option<&Pd2Manifest>,
) -> Result<(), TransactionError> {
    let observed_slots = validate_staging_inventory(staging)?;
    if observed_slots != state.slot_count {
        return Err(TransactionError::Indeterminate);
    }
    if !staging_identities_match(staging, &state.slot_identities)? {
        return Err(TransactionError::Indeterminate);
    }
    if let Some(manifest) = manifest {
        require_staging_manifest(state, manifest)?;
    }
    for directory_name in ["files", ".partial"] {
        let directory = staging
            .open_directory(Path::new(directory_name))
            .map_err(map_safe_io)?;
        for name in directory.names().map_err(map_safe_io)? {
            let file = directory.open_file(Path::new(&name)).map_err(map_safe_io)?;
            directory
                .truncate_open_file(&name, &file)
                .map_err(map_safe_io)?;
        }
    }
    let manifest_file = staging
        .open_file(Path::new(STAGING_MANIFEST))
        .map_err(map_safe_io)?;
    if let Some(manifest) = manifest {
        let observed = read_bounded_open(manifest_file.try_clone()?, 1024 * 1024)?;
        if observed != manifest.raw() {
            return Err(TransactionError::Indeterminate);
        }
    }
    staging
        .truncate_open_file(OsStr::new(STAGING_MANIFEST), &manifest_file)
        .map_err(map_safe_io)
}

fn reclaimed_staging_is_clean(staging: &Directory) -> Result<(), TransactionError> {
    idle_staging_is_empty(staging, &read_staging_state(staging)?)
}

#[cfg(test)]
thread_local! {
    /// This test thread plays the Android host for
    /// [`copied_idle_slots_accepted`].
    static ANDROID_HOST_FOR_TEST: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Only the Android host, where a Launcher folder copied from another device
/// is how an installation arrives, accepts an idle update namespace whose
/// empty slots have other identities than recorded. The desktop keeps
/// comparing every slot identity strictly.
fn copied_idle_slots_accepted() -> bool {
    #[cfg(test)]
    if ANDROID_HOST_FOR_TEST.with(std::cell::Cell::get) {
        return true;
    }
    crate::android_host()
}

/// An idle container: inactive, every slot an empty private file and no
/// staged manifest. On the Android host its slot identities vouch for
/// nothing then, so a copy of it (new inodes, perhaps another filesystem: a
/// Launcher folder copied from another device) is as clean as the original,
/// and its next use rebinds them; the desktop still requires the recorded
/// identities.
fn idle_staging_is_empty(
    staging: &Directory,
    state: &StagingState,
) -> Result<(), TransactionError> {
    let observed_slots = validate_staging_inventory(staging)?;
    if observed_slots != state.slot_count {
        return Err(TransactionError::Indeterminate);
    }
    if !copied_idle_slots_accepted() && !staging_identities_match(staging, &state.slot_identities)?
    {
        return Err(TransactionError::Indeterminate);
    }
    if state.phase != StagingPhase::Inactive {
        return Err(TransactionError::Indeterminate);
    }
    for directory_name in ["files", ".partial"] {
        let directory = staging
            .open_directory(Path::new(directory_name))
            .map_err(map_safe_io)?;
        require_slots_empty(&directory)?;
    }
    let manifest = staging
        .open_file(Path::new(STAGING_MANIFEST))
        .map_err(map_safe_io)?;
    if manifest.metadata()?.len() != 0 {
        return Err(TransactionError::Indeterminate);
    }
    Ok(())
}

pub(crate) struct ExclusiveLease {
    file: File,
    directories: LayoutDirectories,
}

impl ExclusiveLease {
    fn acquire(directories: LayoutDirectories) -> Result<Self, TransactionError> {
        let file = directories
            .metadata
            .open_or_create_file(OsStr::new("installation.lock"))
            .map_err(map_safe_io)?;
        File::try_lock(&file).map_err(|error| match error {
            std::fs::TryLockError::WouldBlock => TransactionError::Busy,
            std::fs::TryLockError::Error(error) => TransactionError::Io(error),
        })?;
        Ok(Self { file, directories })
    }

    pub(crate) fn project_directory(&self) -> &Directory {
        &self.directories.project
    }

    pub(crate) fn metadata_directory(&self) -> &Directory {
        &self.directories.metadata
    }

    pub(crate) fn manifest_staging(
        &self,
        manifest_id: &str,
    ) -> Result<Directory, TransactionError> {
        validate_identifier(manifest_id)?;
        let staging = self
            .directories
            .staging
            .open_directory(Path::new(STAGING_CONTAINER))
            .map_err(map_safe_io)?;
        let state = read_staging_state(&staging)?;
        if !state.phase.is_download_intent() || state.manifest_id != manifest_id {
            return Err(TransactionError::Indeterminate);
        }
        Ok(staging)
    }

    pub(crate) fn staging_slots(
        &self,
        manifest_id: &str,
        path: &str,
    ) -> Result<(String, String), TransactionError> {
        let staging = self.manifest_staging(manifest_id)?;
        let state = read_staging_state(&staging)?;
        let entry = staging_entry(&state, path)?;
        Ok((entry.object_slot.clone(), entry.partial_slot.clone()))
    }

    pub(crate) fn refresh_staging_slot_identity(
        &self,
        manifest_id: &str,
        path: &str,
    ) -> Result<(), TransactionError> {
        let staging = self.manifest_staging(manifest_id)?;
        let mut state = read_staging_state(&staging)?;
        let index = state
            .entries
            .iter()
            .position(|entry| entry.path == path)
            .ok_or(TransactionError::InvalidJournal)?;
        let files = staging
            .open_directory(Path::new("files"))
            .map_err(map_safe_io)?;
        let partials = staging
            .open_directory(Path::new(".partial"))
            .map_err(map_safe_io)?;
        let object = files
            .open_file(Path::new(&state.entries[index].object_slot))
            .map_err(map_safe_io)?;
        let partial = partials
            .open_file(Path::new(&state.entries[index].partial_slot))
            .map_err(map_safe_io)?;
        state.slot_identities[index] = StagingSlotIdentity {
            object: FileIdentity::of(&object).map_err(map_safe_io)?,
            partial: FileIdentity::of(&partial).map_err(map_safe_io)?,
        };
        state.generation = state
            .generation
            .checked_add(1)
            .ok_or(TransactionError::InvalidJournal)?;
        write_staging_state(&staging, &state)
    }
}

impl Drop for ExclusiveLease {
    fn drop(&mut self) {
        let _ = File::unlock(&self.file);
    }
}

fn prepare_layout(layout: &UpdateLayout) -> Result<LayoutDirectories, TransactionError> {
    let project = layout.open_project().map_err(map_safe_io)?;
    let metadata = project
        .ensure_directory(Path::new(".pd2launcher"))
        .map_err(map_safe_io)?;
    let transactions = metadata
        .ensure_directory(Path::new("transactions"))
        .map_err(map_safe_io)?;
    let staging = metadata
        .ensure_directory(Path::new("staging"))
        .map_err(map_safe_io)?;
    let backups = metadata
        .ensure_directory(Path::new("backups"))
        .map_err(map_safe_io)?;
    Ok(LayoutDirectories {
        project,
        metadata,
        transactions,
        staging,
        backups,
    })
}

fn open_existing_layout(
    layout: &UpdateLayout,
) -> Result<Option<LayoutDirectories>, TransactionError> {
    let project = match layout.open_project() {
        Ok(project) => project,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(map_safe_io(error)),
    };
    let metadata = match project.open_directory(Path::new(".pd2launcher")) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(map_safe_io(error)),
    };
    let transactions = match metadata.open_directory(Path::new("transactions")) {
        Ok(transactions) => transactions,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(map_safe_io(error)),
    };
    let staging = match metadata.open_directory(Path::new("staging")) {
        Ok(staging) => staging,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(map_safe_io(error)),
    };
    let backups = match metadata.open_directory(Path::new("backups")) {
        Ok(backups) => backups,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(map_safe_io(error)),
    };
    Ok(Some(LayoutDirectories {
        project,
        metadata,
        transactions,
        staging,
        backups,
    }))
}

fn validate_identifier(identifier: &str) -> Result<(), TransactionError> {
    let valid = !identifier.is_empty()
        && identifier.len() <= 128
        && identifier
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'));
    if valid {
        Ok(())
    } else {
        Err(TransactionError::InvalidIdentifier)
    }
}

fn backup_object_name(index: usize) -> String {
    numbered_name(BACKUP_OBJECT_PREFIX, index)
}

fn numbered_name(prefix: &str, index: usize) -> String {
    format!("{prefix}{index:04}")
}

fn is_numbered_name(name: &str, prefix: &str) -> bool {
    let Some(index) = name.strip_prefix(prefix) else {
        return false;
    };
    index.len() == 4
        && index.bytes().all(|byte| byte.is_ascii_digit())
        && index.parse::<usize>().is_ok_and(|index| index < 4096)
}

fn validate_backup_inventory(directory: &Directory) -> Result<usize, TransactionError> {
    let mut indices = Vec::new();
    for name in directory.names().map_err(map_safe_io)? {
        let Some(name_text) = name.to_str() else {
            return Err(TransactionError::Indeterminate);
        };
        if !is_numbered_name(name_text, BACKUP_OBJECT_PREFIX) {
            return Err(TransactionError::Indeterminate);
        }
        directory.open_file(Path::new(&name)).map_err(map_safe_io)?;
        indices.push(
            name_text
                .strip_prefix(BACKUP_OBJECT_PREFIX)
                .and_then(|index| index.parse::<usize>().ok())
                .ok_or(TransactionError::Indeterminate)?,
        );
    }
    indices.sort_unstable();
    if indices.iter().copied().ne(0..indices.len()) {
        return Err(TransactionError::Indeterminate);
    }
    Ok(indices.len())
}

fn collect_backup_identities(
    directory: &Directory,
    count: usize,
) -> Result<Vec<FileIdentity>, TransactionError> {
    (0..count)
        .map(|index| {
            let file = directory
                .open_file(Path::new(&backup_object_name(index)))
                .map_err(map_safe_io)?;
            FileIdentity::of(&file).map_err(map_safe_io)
        })
        .collect()
}

fn require_inactive_backup_slots_empty(
    directory: &Directory,
    journal: &Journal,
) -> Result<(), TransactionError> {
    if validate_backup_inventory(directory)? != journal.backup_slot_count {
        return Err(TransactionError::Indeterminate);
    }
    for index in 0..journal.backup_slot_count {
        let name = backup_object_name(index);
        let file = directory.open_file(Path::new(&name)).map_err(map_safe_io)?;
        let observed_identity = FileIdentity::of(&file).map_err(map_safe_io)?;
        if observed_identity != journal.backup_identities[index] {
            let Some(action) = journal.actions.iter().find(|action| {
                !action.backed_up
                    && action.previous_md5.is_some()
                    && action.backup_slot.as_deref() == Some(name.as_str())
            }) else {
                return Err(TransactionError::Indeterminate);
            };
            let Some(previous) = action.previous_md5.as_deref() else {
                return Err(TransactionError::Indeterminate);
            };
            let mut payload = file.try_clone().map_err(map_safe_io)?;
            verify_open_md5(&mut payload, previous)?;
            if !directory
                .atomic_publication_matches(OsStr::new(&name), &file)
                .map_err(map_safe_io)?
            {
                return Err(TransactionError::Indeterminate);
            }
            continue;
        }
        let active = journal.actions.iter().any(|action| {
            action.previous_md5.is_some()
                && action.backed_up
                && action.backup_slot.as_deref() == Some(name.as_str())
        });
        if !active && file.metadata()?.len() != 0 {
            return Err(TransactionError::Indeterminate);
        }
    }
    Ok(())
}

fn require_all_backup_slots_empty(directory: &Directory) -> Result<(), TransactionError> {
    for name in directory.names().map_err(map_safe_io)? {
        let file = directory.open_file(Path::new(&name)).map_err(map_safe_io)?;
        if file.metadata()?.len() != 0 {
            return Err(TransactionError::Indeterminate);
        }
    }
    Ok(())
}

#[cfg(test)]
fn atomic_write(path: PathBuf, bytes: &[u8]) -> Result<(), TransactionError> {
    reject_symlink_if_present(&path)?;
    let file_name = path.file_name().ok_or(TransactionError::UnsafePath)?;
    Directory::open(path.parent().ok_or(TransactionError::UnsafePath)?)?
        .atomic_write(file_name, bytes)
        .map_err(TransactionError::Io)
}

#[cfg(test)]
fn ensure_open_identity(path: &Path, file: &File) -> Result<(), TransactionError> {
    let path_metadata = fs::symlink_metadata(path)?;
    let file_metadata = file.metadata()?;
    if path_metadata.file_type().is_symlink()
        || !path_metadata.is_file()
        || path_metadata.dev() != file_metadata.dev()
        || path_metadata.ino() != file_metadata.ino()
        || path_metadata.nlink() != 1
    {
        return Err(TransactionError::UnsafePath);
    }
    Ok(())
}

fn map_safe_io(error: io::Error) -> TransactionError {
    if matches!(
        error.kind(),
        io::ErrorKind::InvalidData | io::ErrorKind::InvalidInput
    ) || error.raw_os_error() == Some(rustix::io::Errno::LOOP.raw_os_error())
    {
        TransactionError::UnsafePath
    } else {
        TransactionError::Io(error)
    }
}

fn reject_cancelled(control: &OperationControl) -> Result<(), TransactionError> {
    if control.is_cancelled() {
        Err(TransactionError::Cancelled)
    } else {
        Ok(())
    }
}

fn read_bounded_open(file: File, limit: u64) -> Result<Vec<u8>, TransactionError> {
    if file.metadata()?.len() > limit {
        return Err(TransactionError::InvalidJournal);
    }
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(TransactionError::InvalidJournal);
    }
    Ok(bytes)
}

fn file_md5_open(file: &mut File) -> Result<[u8; 16], TransactionError> {
    let mut hasher = Md5::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize().into())
}

fn verify_open_md5(file: &mut File, expected: &str) -> Result<(), TransactionError> {
    if file_md5_open(file)? == parse_md5_hex(expected)? {
        Ok(())
    } else {
        Err(TransactionError::DigestMismatch)
    }
}

fn open_optional_file(
    directory: &Directory,
    name: &OsStr,
) -> Result<Option<File>, TransactionError> {
    match directory.open_file(Path::new(name)) {
        Ok(file) => Ok(Some(file)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(map_safe_io(error)),
    }
}

fn digest_matches_at(
    directory: &Directory,
    name: &OsStr,
    expected: &str,
) -> Result<bool, TransactionError> {
    let Some(mut file) = open_optional_file(directory, name)? else {
        return Ok(false);
    };
    Ok(file_md5_open(&mut file)? == parse_md5_hex(expected)?)
}

fn parse_md5_hex(value: &str) -> Result<[u8; 16], TransactionError> {
    if value.len() != 32 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(TransactionError::InvalidJournal);
    }
    let mut output = [0_u8; 16];
    for (index, output) in output.iter_mut().enumerate() {
        *output = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)
            .map_err(|_| TransactionError::InvalidJournal)?;
    }
    Ok(output)
}

fn reject_symlink_if_present(path: &Path) -> Result<(), TransactionError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(TransactionError::UnsafePath),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(TransactionError::Io(error)),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        cell::{Cell, RefCell},
        ffi::OsString,
        fs::OpenOptions,
        os::unix::fs::symlink,
        sync::mpsc,
        thread,
        time::Duration,
    };

    use tempfile::TempDir;

    use super::*;
    use crate::manifest::{ManifestResponse, PD2_MANIFEST_URL};

    struct FailAt {
        target: usize,
        current: Cell<usize>,
    }

    struct FailAtFilesystemMutation {
        target: usize,
        current: Cell<usize>,
    }

    #[derive(Default)]
    struct RecordAtomicMutations {
        observed: RefCell<Vec<(AtomicSurface, AtomicMutation)>>,
    }

    struct FailAtAtomicMutation {
        surface: AtomicSurface,
        target: usize,
        current: Cell<usize>,
    }

    struct CancelAfterMutation {
        control: OperationControl,
        requested: Cell<bool>,
    }

    #[test]
    fn transaction_layout_rejects_a_symlinked_managed_root_ancestor() {
        let fixture = TempDir::new().unwrap();
        let outside = fixture.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        symlink(&outside, fixture.path().join("redirect")).unwrap();
        let layout = UpdateLayout::new(fixture.path().join("redirect/ProjectD2"));

        assert!(matches!(
            layout.acquire_exclusive(),
            Err(TransactionError::UnsafePath)
        ));
        assert!(std::fs::read_dir(outside).unwrap().next().is_none());
    }

    #[test]
    fn anchored_layout_keeps_probe_lock_and_staging_on_the_retained_root() {
        let fixture = TempDir::new().unwrap();
        let managed = fixture.path().join("managed");
        let project = managed.join("ProjectD2");
        let outside = fixture.path().join("outside");
        fs::create_dir_all(&project).unwrap();
        fs::create_dir(&outside).unwrap();
        let root = Directory::open(&managed).unwrap();
        let layout = UpdateLayout::anchored(project.clone(), root, PathBuf::from("ProjectD2"));

        let retained = fixture.path().join("managed-away");
        fs::rename(&managed, &retained).unwrap();
        symlink(&outside, &managed).unwrap();
        let manifest = manifest_for("Game.exe", b"new game");
        layout.prepare_staging(&manifest).unwrap();
        let lease = layout.acquire_exclusive().unwrap();
        drop(lease);

        assert!(retained.join("ProjectD2/.pd2launcher/staging").is_dir());
        assert!(fs::read_dir(outside).unwrap().next().is_none());
        assert_eq!(
            UpdateTransactionProbe::new(layout).probe().unwrap(),
            TransactionEvidence::Repairable,
            "durable unjournaled staging must not be classified clear"
        );
    }

    impl FaultInjector for CancelAfterMutation {
        fn after(&self, event: FaultEvent) -> io::Result<()> {
            if event == FaultEvent::FilesystemMutation && !self.requested.replace(true) {
                self.control.request_cancel();
            }
            Ok(())
        }
    }

    impl FaultInjector for FailAt {
        fn after(&self, _event: FaultEvent) -> io::Result<()> {
            let current = self.current.get() + 1;
            self.current.set(current);
            if current == self.target {
                Err(io::Error::new(io::ErrorKind::Interrupted, "injected stop"))
            } else {
                Ok(())
            }
        }
    }

    impl FaultInjector for FailAtFilesystemMutation {
        fn after(&self, event: FaultEvent) -> io::Result<()> {
            if event != FaultEvent::FilesystemMutation {
                return Ok(());
            }
            let current = self.current.get() + 1;
            self.current.set(current);
            if current == self.target {
                Err(io::Error::new(io::ErrorKind::Interrupted, "injected stop"))
            } else {
                Ok(())
            }
        }
    }

    impl FaultInjector for RecordAtomicMutations {
        fn after(&self, _event: FaultEvent) -> io::Result<()> {
            Ok(())
        }

        fn atomic_after(&self, surface: AtomicSurface, event: AtomicMutation) -> io::Result<()> {
            self.observed.borrow_mut().push((surface, event));
            Ok(())
        }
    }

    impl FaultInjector for FailAtAtomicMutation {
        fn after(&self, _event: FaultEvent) -> io::Result<()> {
            Ok(())
        }

        fn atomic_after(&self, surface: AtomicSurface, _event: AtomicMutation) -> io::Result<()> {
            if surface != self.surface {
                return Ok(());
            }
            let current = self.current.get() + 1;
            self.current.set(current);
            if current == self.target {
                Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "injected nested atomic publisher stop",
                ))
            } else {
                Ok(())
            }
        }
    }

    fn grammar_identity(value: u64) -> FileIdentity {
        FileIdentity::for_test(1, value)
    }

    fn grammar_journal(state: JournalState) -> Journal {
        Journal {
            schema: JOURNAL_SCHEMA,
            publication_generation: 1,
            transaction_id: "grammar-transaction".to_owned(),
            manifest_id: "grammar-manifest".to_owned(),
            manifest_sha256: Some("11".repeat(32)),
            state,
            backup_slot_count: 2,
            backup_identities: vec![grammar_identity(10), grammar_identity(11)],
            backup_growth: None,
            actions: vec![
                ActionRecord {
                    path: "Game.exe".to_owned(),
                    expected_md5: "22".repeat(16),
                    previous_md5: Some("33".repeat(16)),
                    backup_slot: Some(backup_object_name(0)),
                    backed_up: false,
                    promoted: false,
                },
                ActionRecord {
                    path: "ProjectD2.dll".to_owned(),
                    expected_md5: "44".repeat(16),
                    previous_md5: None,
                    backup_slot: Some(backup_object_name(1)),
                    backed_up: false,
                    promoted: false,
                },
            ],
        }
    }

    fn initial_growth_journal() -> Journal {
        let mut journal = grammar_journal(JournalState::Planned);
        journal.backup_slot_count = 0;
        journal.backup_identities.clear();
        journal.backup_growth = Some(BackupGrowth {
            old_slots: 0,
            new_slots: journal.actions.len(),
            identities: Vec::new(),
        });
        journal
    }

    #[test]
    fn backup_growth_reconciliation_accepts_zero_or_one_unrecorded_identity_only() {
        for unrecorded_slots in 0..=2 {
            let fixture = TempDir::new().unwrap();
            let layout = UpdateLayout::new(fixture.path());
            let directories = prepare_layout(&layout).unwrap();
            let backup = directories
                .backups
                .ensure_directory(Path::new(TRANSACTION_BACKUP_SLOT))
                .unwrap();
            let manager = TransactionManager::new(&layout, &NoFaults);
            let mut journal = initial_growth_journal();
            manager.write_journal(&directories, &mut journal).unwrap();
            let published = journal.publication_generation;

            for index in 0..unrecorded_slots {
                backup
                    .open_or_create_file(OsStr::new(&backup_object_name(index)))
                    .unwrap();
            }
            backup.sync().unwrap();

            let result = manager.reconcile_backup_growth(&directories, &mut journal);
            if unrecorded_slots <= 1 {
                result.unwrap_or_else(|error| {
                    panic!(
                        "rejected {unrecorded_slots} unrecorded backup slot identities: {error:?}"
                    )
                });
                assert_eq!(journal.backup_slot_count, journal.actions.len());
                assert_eq!(journal.backup_identities.len(), journal.actions.len());
                assert!(journal.backup_growth.is_none());
                assert_eq!(
                    read_journals_from(&directories.transactions).unwrap(),
                    vec![journal]
                );
            } else {
                assert!(matches!(result, Err(TransactionError::Indeterminate)));
                assert_eq!(journal.publication_generation, published);
                assert!(journal.backup_identities.is_empty());
                assert_eq!(
                    read_journals_from(&directories.transactions).unwrap()[0].backup_identities,
                    Vec::<FileIdentity>::new()
                );
                assert_eq!(validate_backup_inventory(&backup).unwrap(), 2);
                assert!(matches!(
                    TransactionManager::new(&layout, &NoFaults).repair(),
                    Err(TransactionError::Indeterminate)
                ));
            }
        }
    }

    #[test]
    fn every_transaction_checkpoint_repairs_to_old_or_fully_committed_new_content() {
        for fault in 1..=10 {
            let temporary = TempDir::new().unwrap();
            let layout = UpdateLayout::new(temporary.path());
            let manifest = manifest_for("Game.exe", b"new game");
            layout.prepare_staging(&manifest).unwrap();
            fs::write(
                layout.staged_files(manifest.id()).join("object-0000"),
                b"new game",
            )
            .unwrap();
            fs::write(temporary.path().join("Game.exe"), b"old game").unwrap();
            let fail = FailAt {
                target: fault,
                current: Cell::new(0),
            };
            let promotion = Promotion {
                path: RelativeGamePath::parse("Game.exe").unwrap(),
                expected_md5: Md5::digest(b"new game").into(),
            };

            let result = TransactionManager::new(&layout, &fail).promote(
                "fault-test",
                &manifest,
                &[promotion],
            );
            assert!(result.is_err(), "fault checkpoint {fault} was not reached");

            TransactionManager::new(&layout, &NoFaults)
                .repair()
                .unwrap_or_else(|error| panic!("checkpoint {fault} repair failed: {error:?}"));
            let final_bytes = fs::read(temporary.path().join("Game.exe")).unwrap();
            assert!(
                final_bytes == b"old game" || final_bytes == b"new game",
                "checkpoint {fault} produced mixed content"
            );
            if final_bytes == b"new game" {
                assert_eq!(
                    fs::read(layout.accepted_manifest()).unwrap(),
                    manifest.raw()
                );
            } else {
                assert!(!layout.accepted_manifest().exists());
            }
            assert_eq!(
                UpdateTransactionProbe::new(layout.clone()).probe().unwrap(),
                TransactionEvidence::Clear
            );
        }
    }

    #[test]
    fn every_nested_project_backup_and_journal_publication_cut_repairs_before_clear() {
        let recorded_fixture = TempDir::new().unwrap();
        let recorded_layout = UpdateLayout::new(recorded_fixture.path());
        let recorded_manifest = manifest_for("Game.exe", b"new game");
        recorded_layout.prepare_staging(&recorded_manifest).unwrap();
        fs::write(
            recorded_layout
                .staged_files(recorded_manifest.id())
                .join("object-0000"),
            b"new game",
        )
        .unwrap();
        fs::write(recorded_fixture.path().join("Game.exe"), b"old game").unwrap();
        let recorder = RecordAtomicMutations::default();
        TransactionManager::new(&recorded_layout, &recorder)
            .promote(
                "nested-record",
                &recorded_manifest,
                &[Promotion {
                    path: RelativeGamePath::parse("Game.exe").unwrap(),
                    expected_md5: Md5::digest(b"new game").into(),
                }],
            )
            .unwrap();
        let observed = recorder.observed.into_inner();

        for surface in [
            AtomicSurface::Backup,
            AtomicSurface::Project,
            AtomicSurface::Journal,
        ] {
            let surface_events = observed
                .iter()
                .filter_map(|(observed_surface, event)| {
                    (*observed_surface == surface).then_some(*event)
                })
                .collect::<Vec<_>>();
            assert!(
                surface_events.contains(&AtomicMutation::Prepared),
                "{surface:?} wrote nothing"
            );
            // Every step of every write on this surface: the temporary file
            // on disk, the rename, and the directory sync.
            for target in 1..=surface_events.len() {
                let fixture = TempDir::new().unwrap();
                let layout = UpdateLayout::new(fixture.path());
                let manifest = manifest_for("Game.exe", b"new game");
                layout.prepare_staging(&manifest).unwrap();
                fs::write(
                    layout.staged_files(manifest.id()).join("object-0000"),
                    b"new game",
                )
                .unwrap();
                fs::write(fixture.path().join("Game.exe"), b"old game").unwrap();
                let fail = FailAtAtomicMutation {
                    surface,
                    target,
                    current: Cell::new(0),
                };
                let result = TransactionManager::new(&layout, &fail).promote(
                    "nested-cut",
                    &manifest,
                    &[Promotion {
                        path: RelativeGamePath::parse("Game.exe").unwrap(),
                        expected_md5: Md5::digest(b"new game").into(),
                    }],
                );
                assert!(
                    result.is_err(),
                    "{surface:?} nested publisher cut {target} was not reached"
                );
                assert_eq!(
                    UpdateTransactionProbe::new(layout.clone()).probe().unwrap(),
                    TransactionEvidence::Repairable,
                    "{surface:?} nested publisher cut {target} was not repairable"
                );
                assert_eq!(
                    TransactionManager::new(&layout, &NoFaults)
                        .repair()
                        .unwrap_or_else(|error| panic!(
                            "{surface:?} nested publisher cut {target} failed repair: {error:?}"
                        )),
                    RepairOutcome::Repaired(1)
                );
                assert_eq!(
                    UpdateTransactionProbe::new(layout.clone()).probe().unwrap(),
                    TransactionEvidence::Clear,
                    "{surface:?} nested publisher cut {target} was not normalized"
                );
                let final_bytes = fs::read(fixture.path().join("Game.exe")).unwrap();
                assert!(final_bytes == b"old game" || final_bytes == b"new game");
                if final_bytes == b"new game" {
                    assert_eq!(
                        fs::read(layout.accepted_manifest()).unwrap(),
                        manifest.raw()
                    );
                } else {
                    assert!(!layout.accepted_manifest().exists());
                }
            }
        }
    }

    #[test]
    fn every_terminal_prune_journal_publication_cut_is_repairable_or_complete() {
        let recorded_fixture = TempDir::new().unwrap();
        let recorded_layout = UpdateLayout::new(recorded_fixture.path());
        let recorded_manifest = manifest_for("Game.exe", b"new game");
        recorded_layout.prepare_staging(&recorded_manifest).unwrap();
        fs::write(
            recorded_layout
                .staged_files(recorded_manifest.id())
                .join("object-0000"),
            b"new game",
        )
        .unwrap();
        fs::write(recorded_fixture.path().join("Game.exe"), b"old game").unwrap();
        TransactionManager::new(&recorded_layout, &NoFaults)
            .promote(
                "prune-record",
                &recorded_manifest,
                &[Promotion {
                    path: RelativeGamePath::parse("Game.exe").unwrap(),
                    expected_md5: Md5::digest(b"new game").into(),
                }],
            )
            .unwrap();
        let recorder = RecordAtomicMutations::default();
        TransactionManager::new(&recorded_layout, &recorder)
            .prune_terminal(
                PruneAuditEvidence::FreshAndClear,
                &CleanupLease::acquire(&recorded_fixture.path().join("cleanup.lock")).unwrap(),
            )
            .unwrap();
        let events = recorder
            .observed
            .into_inner()
            .into_iter()
            .filter_map(|(surface, event)| (surface == AtomicSurface::Journal).then_some(event))
            .collect::<Vec<_>>();
        assert!(events.contains(&AtomicMutation::Prepared));
        for target in 1..=events.len() {
            let fixture = TempDir::new().unwrap();
            let layout = UpdateLayout::new(fixture.path());
            let manifest = manifest_for("Game.exe", b"new game");
            layout.prepare_staging(&manifest).unwrap();
            fs::write(
                layout.staged_files(manifest.id()).join("object-0000"),
                b"new game",
            )
            .unwrap();
            fs::write(fixture.path().join("Game.exe"), b"old game").unwrap();
            TransactionManager::new(&layout, &NoFaults)
                .promote(
                    "prune-cut",
                    &manifest,
                    &[Promotion {
                        path: RelativeGamePath::parse("Game.exe").unwrap(),
                        expected_md5: Md5::digest(b"new game").into(),
                    }],
                )
                .unwrap();
            let fail = FailAtAtomicMutation {
                surface: AtomicSurface::Journal,
                target,
                current: Cell::new(0),
            };
            let stopped = TransactionManager::new(&layout, &fail).prune_terminal(
                PruneAuditEvidence::FreshAndClear,
                &CleanupLease::acquire(&fixture.path().join("cleanup.lock")).unwrap(),
            );
            assert!(
                stopped.is_err(),
                "prune journal cut {target} was not reached"
            );

            match UpdateTransactionProbe::new(layout.clone()).probe().unwrap() {
                TransactionEvidence::Repairable => {
                    let repair = TransactionManager::new(&layout, &NoFaults).repair();
                    assert!(
                        matches!(repair, Ok(RepairOutcome::Repaired(_))),
                        "prune journal cut {target} ({:?}) failed repair: {repair:?}",
                        events.get(target.saturating_sub(1))
                    );
                    assert_eq!(
                        UpdateTransactionProbe::new(layout).probe().unwrap(),
                        TransactionEvidence::Clear
                    );
                }
                TransactionEvidence::Clear => {
                    let directories = open_existing_layout(&layout).unwrap().unwrap();
                    let journals = read_journals_from(&directories.transactions).unwrap();
                    assert!(matches!(journals[0].state, JournalState::Pruned));
                }
                TransactionEvidence::Unsafe => {
                    let directories = open_existing_layout(&layout).unwrap().unwrap();
                    panic!(
                        "prune journal cut {target} ({:?}) became unsafe: {:?}",
                        events.get(target.saturating_sub(1)),
                        inspect_pending_journal(&directories)
                    )
                }
            }
        }
    }

    #[test]
    fn unknown_final_content_and_unsafe_journal_paths_are_indeterminate() {
        let temporary = TempDir::new().unwrap();
        let layout = UpdateLayout::new(temporary.path());
        let manifest = manifest_for("Game.exe", b"new game");
        layout.prepare_staging(&manifest).unwrap();
        fs::write(
            layout.staged_files(manifest.id()).join("object-0000"),
            b"new game",
        )
        .unwrap();
        fs::write(temporary.path().join("Game.exe"), b"old game").unwrap();
        let fail = FailAt {
            target: 3,
            current: Cell::new(0),
        };
        let promotion = Promotion {
            path: RelativeGamePath::parse("Game.exe").unwrap(),
            expected_md5: Md5::digest(b"new game").into(),
        };
        TransactionManager::new(&layout, &fail)
            .promote("unsafe-test", &manifest, &[promotion])
            .unwrap_err();
        fs::write(temporary.path().join("Game.exe"), b"foreign bytes").unwrap();

        assert_eq!(
            UpdateTransactionProbe::new(layout.clone()).probe().unwrap(),
            TransactionEvidence::Unsafe
        );
        assert!(matches!(
            TransactionManager::new(&layout, &NoFaults).repair(),
            Err(TransactionError::Indeterminate | TransactionError::UnsafePath)
        ));

        let linked = TempDir::new().unwrap();
        let linked_layout = UpdateLayout::new(linked.path());
        fs::create_dir(linked.path().join("actual")).unwrap();
        fs::create_dir(linked.path().join(".pd2launcher")).unwrap();
        symlink(
            linked.path().join("actual"),
            linked_layout.transactions_directory(),
        )
        .unwrap();
        assert_eq!(
            UpdateTransactionProbe::new(linked_layout).probe().unwrap(),
            TransactionEvidence::Unsafe
        );
    }

    #[test]
    fn cancellation_after_first_mutation_finishes_a_terminal_transaction() {
        let temporary = TempDir::new().unwrap();
        let layout = UpdateLayout::new(temporary.path());
        let manifest = manifest_for("Game.exe", b"new game");
        layout.prepare_staging(&manifest).unwrap();
        fs::write(
            layout.staged_files(manifest.id()).join("object-0000"),
            b"new game",
        )
        .unwrap();
        fs::write(temporary.path().join("Game.exe"), b"old game").unwrap();
        let control = OperationControl::default();
        let faults = CancelAfterMutation {
            control: control.clone(),
            requested: Cell::new(false),
        };
        let manager = TransactionManager::new(&layout, &faults);
        let lease = layout.acquire_exclusive().unwrap();
        let error = manager
            .promote_with_lease_controlled(
                "cancel-safe",
                &manifest,
                &[Promotion {
                    path: RelativeGamePath::parse("Game.exe").unwrap(),
                    expected_md5: Md5::digest(b"new game").into(),
                }],
                &lease,
                &control,
            )
            .unwrap_err();
        drop(lease);

        assert!(matches!(error, TransactionError::Cancelled));
        assert_eq!(
            fs::read(temporary.path().join("Game.exe")).unwrap(),
            b"new game"
        );
        assert_eq!(
            UpdateTransactionProbe::new(layout).probe().unwrap(),
            TransactionEvidence::Clear
        );
    }

    #[test]
    fn retained_update_lease_prevents_managed_root_swap_from_redirecting_promotion() {
        let fixture = TempDir::new().unwrap();
        let project = fixture.path().join("ProjectD2");
        let outside = fixture.path().join("outside");
        fs::create_dir(&project).unwrap();
        fs::create_dir(&outside).unwrap();
        let layout = UpdateLayout::new(&project);
        let manifest = manifest_for("Game.exe", b"new game");
        layout.prepare_staging(&manifest).unwrap();
        fs::write(
            layout.staged_files(manifest.id()).join("object-0000"),
            b"new game",
        )
        .unwrap();
        fs::write(project.join("Game.exe"), b"old game").unwrap();
        let lease = layout.acquire_exclusive().unwrap();

        let retained = fixture.path().join("ProjectD2-retained");
        fs::rename(&project, &retained).unwrap();
        symlink(&outside, &project).unwrap();
        TransactionManager::new(&layout, &NoFaults)
            .promote_with_lease(
                "root-swap",
                &manifest,
                &[Promotion {
                    path: RelativeGamePath::parse("Game.exe").unwrap(),
                    expected_md5: Md5::digest(b"new game").into(),
                }],
                &lease,
            )
            .unwrap();

        assert_eq!(fs::read(retained.join("Game.exe")).unwrap(), b"new game");
        assert!(fs::read_dir(outside).unwrap().next().is_none());
    }

    #[test]
    fn atomic_write_uses_an_unpredictable_exclusive_temp_and_never_writes_through_links() {
        let temporary = TempDir::new().unwrap();
        let target = temporary.path().join("state.json");
        let outside = temporary.path().join("outside");
        fs::write(&outside, b"outside").unwrap();
        let predictable = temporary.path().join("state.json.tmp");
        symlink(&outside, &predictable).unwrap();

        atomic_write(target.clone(), b"durable").unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"durable");
        assert_eq!(fs::read(&outside).unwrap(), b"outside");
        assert!(
            fs::symlink_metadata(&predictable)
                .unwrap()
                .file_type()
                .is_symlink()
        );

        let linked_target = temporary.path().join("linked.json");
        fs::hard_link(&outside, &linked_target).unwrap();
        // The rename replaces the name, never the file it was linked to.
        atomic_write(linked_target.clone(), b"replacement").unwrap();
        assert_eq!(fs::read(&linked_target).unwrap(), b"replacement");
        assert_eq!(fs::read(&outside).unwrap(), b"outside");

        let symlink_target = temporary.path().join("symlink.json");
        symlink(&outside, &symlink_target).unwrap();
        assert!(matches!(
            atomic_write(symlink_target, b"blocked"),
            Err(TransactionError::UnsafePath)
        ));

        let substituted = temporary.path().join("substituted.tmp");
        let opened = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&substituted)
            .unwrap();
        let moved = temporary.path().join("substituted-moved.tmp");
        fs::rename(&substituted, &moved).unwrap();
        symlink(&moved, &substituted).unwrap();
        assert!(matches!(
            ensure_open_identity(&substituted, &opened),
            Err(TransactionError::UnsafePath)
        ));

        fs::remove_file(&substituted).unwrap();
        fs::rename(&moved, &substituted).unwrap();
        fs::hard_link(&substituted, temporary.path().join("substituted-hardlink")).unwrap();
        assert!(matches!(
            ensure_open_identity(&substituted, &opened),
            Err(TransactionError::UnsafePath)
        ));
    }

    #[test]
    fn terminal_material_is_pruned_only_with_clear_audit_and_cleanup_lease() {
        let temporary = TempDir::new().unwrap();
        let layout = UpdateLayout::new(temporary.path());
        let manifest = manifest_for("Game.exe", b"new game");
        layout.prepare_staging(&manifest).unwrap();
        fs::write(
            layout.staged_files(manifest.id()).join("object-0000"),
            b"new game",
        )
        .unwrap();
        fs::write(temporary.path().join("Game.exe"), b"old game").unwrap();
        TransactionManager::new(&layout, &NoFaults)
            .promote(
                "prune-test",
                &manifest,
                &[Promotion {
                    path: RelativeGamePath::parse("Game.exe").unwrap(),
                    expected_md5: Md5::digest(b"new game").into(),
                }],
            )
            .unwrap();
        let journal = layout.journal_path("prune-test");
        let backup = layout.backup_root("prune-test");
        let staging = layout
            .metadata_root()
            .join("staging")
            .join(STAGING_CONTAINER);
        assert!(journal.is_file() && backup.is_dir() && staging.is_dir());

        let manager = TransactionManager::new(&layout, &NoFaults);
        let cleanup = CleanupLease::acquire(&temporary.path().join("cleanup.lock")).unwrap();
        assert_eq!(
            manager
                .prune_terminal(PruneAuditEvidence::FreshAndClear, &cleanup)
                .unwrap(),
            1
        );
        assert!(journal.is_file() && backup.is_dir() && staging.is_dir());
        assert_eq!(fs::metadata(backup.join("object-0000")).unwrap().len(), 0);
        let transactions = Directory::open(&layout.transactions_directory()).unwrap();
        assert!(matches!(
            read_journals_from(&transactions).unwrap()[0].state,
            JournalState::Pruned
        ));
        reclaimed_staging_is_clean(&Directory::open(&staging).unwrap()).unwrap();
    }

    /// A Start over on the Android host that a stop left inside the removal
    /// of the game folder, with the update records of a committed update in
    /// `ProjectD2/.pd2launcher` part way gone: the staging container went
    /// first, the journal is still there. The probe reads it as Unsafe, which
    /// the audit gives as an Indeterminate transaction and which Start over's
    /// preflight refused (`composition::production` `start_over_on` now lets a
    /// Start over that its marker shows as begun go on). Once `transactions`
    /// is gone too, there are no records, and it is clear.
    #[test]
    fn update_records_that_a_removal_cut_part_way_read_as_unsafe() {
        let temporary = TempDir::new().unwrap();
        let layout = UpdateLayout::new(temporary.path());
        let manifest = manifest_for("Game.exe", b"new game");
        layout.prepare_staging(&manifest).unwrap();
        fs::write(
            layout.staged_files(manifest.id()).join("object-0000"),
            b"new game",
        )
        .unwrap();
        fs::write(temporary.path().join("Game.exe"), b"old game").unwrap();
        TransactionManager::new(&layout, &NoFaults)
            .promote(
                "start-over",
                &manifest,
                &[Promotion {
                    path: RelativeGamePath::parse("Game.exe").unwrap(),
                    expected_md5: Md5::digest(b"new game").into(),
                }],
            )
            .unwrap();
        let probe = UpdateTransactionProbe::new(layout.clone());
        assert_eq!(probe.probe().unwrap(), TransactionEvidence::Clear);
        fs::remove_dir_all(
            layout
                .metadata_root()
                .join("staging")
                .join(STAGING_CONTAINER),
        )
        .unwrap();
        assert!(layout.journal_path("start-over").is_file());
        assert_eq!(probe.probe().unwrap(), TransactionEvidence::Unsafe);
        fs::remove_dir_all(layout.transactions_directory()).unwrap();
        assert_eq!(probe.probe().unwrap(), TransactionEvidence::Clear);
    }

    #[test]
    fn repeated_success_and_prune_reuse_fixed_transaction_storage() {
        let temporary = TempDir::new().unwrap();
        let layout = UpdateLayout::new(temporary.path());
        let manifest = manifest_for("Game.exe", b"new game");
        fs::write(temporary.path().join("Game.exe"), b"old game").unwrap();
        let cleanup = CleanupLease::acquire(&temporary.path().join("cleanup.lock")).unwrap();
        let mut stable_entries = None;

        for sequence in 0..128 {
            layout.prepare_staging(&manifest).unwrap();
            fs::write(
                layout.staged_files(manifest.id()).join("object-0000"),
                b"new game",
            )
            .unwrap();
            TransactionManager::new(&layout, &NoFaults)
                .promote(
                    &format!("cycle-{sequence}"),
                    &manifest,
                    &[Promotion {
                        path: RelativeGamePath::parse("Game.exe").unwrap(),
                        expected_md5: Md5::digest(b"new game").into(),
                    }],
                )
                .unwrap();
            assert_eq!(
                TransactionManager::new(&layout, &NoFaults)
                    .prune_terminal(PruneAuditEvidence::FreshAndClear, &cleanup)
                    .unwrap(),
                1
            );

            let entries = count_tree_entries(temporary.path());
            match stable_entries {
                None => stable_entries = Some(entries),
                Some(expected) => assert_eq!(entries, expected),
            }
            let staging = layout
                .metadata_root()
                .join("staging")
                .join(STAGING_CONTAINER);
            reclaimed_staging_is_clean(&Directory::open(&staging).unwrap()).unwrap();
            assert_eq!(
                fs::metadata(layout.backup_root("ignored").join("object-0000"))
                    .unwrap()
                    .len(),
                0
            );
        }
    }

    #[test]
    fn changing_file_sets_reuse_numbered_backups_without_stale_path_poisoning() {
        let temporary = TempDir::new().unwrap();
        let layout = UpdateLayout::new(temporary.path());
        let cleanup = CleanupLease::acquire(&temporary.path().join("cleanup.lock")).unwrap();
        fs::write(temporary.path().join("Game.exe"), b"old game").unwrap();
        fs::write(temporary.path().join("ProjectDiablo.dll"), b"old dll").unwrap();

        let first =
            manifest_for_many(&[("Game.exe", b"game one"), ("ProjectDiablo.dll", b"dll one")]);
        layout.prepare_staging(&first).unwrap();
        fs::write(
            layout.staged_files(first.id()).join("object-0000"),
            b"game one",
        )
        .unwrap();
        fs::write(
            layout.staged_files(first.id()).join("object-0001"),
            b"dll one",
        )
        .unwrap();
        TransactionManager::new(&layout, &NoFaults)
            .promote(
                "two-files",
                &first,
                &[
                    Promotion {
                        path: RelativeGamePath::parse("Game.exe").unwrap(),
                        expected_md5: Md5::digest(b"game one").into(),
                    },
                    Promotion {
                        path: RelativeGamePath::parse("ProjectDiablo.dll").unwrap(),
                        expected_md5: Md5::digest(b"dll one").into(),
                    },
                ],
            )
            .unwrap();
        TransactionManager::new(&layout, &NoFaults)
            .prune_terminal(PruneAuditEvidence::FreshAndClear, &cleanup)
            .unwrap();

        let dll_only = manifest_for("ProjectDiablo.dll", b"dll two");
        layout.prepare_staging(&dll_only).unwrap();
        fs::write(
            layout.staged_files(dll_only.id()).join("object-0000"),
            b"dll two",
        )
        .unwrap();
        TransactionManager::new(&layout, &NoFaults)
            .promote(
                "dll-only",
                &dll_only,
                &[Promotion {
                    path: RelativeGamePath::parse("ProjectDiablo.dll").unwrap(),
                    expected_md5: Md5::digest(b"dll two").into(),
                }],
            )
            .unwrap();
        TransactionManager::new(&layout, &NoFaults)
            .prune_terminal(PruneAuditEvidence::FreshAndClear, &cleanup)
            .unwrap();

        fs::remove_file(temporary.path().join("Game.exe")).unwrap();
        let reintroduced = manifest_for("Game.exe", b"game reintroduced");
        layout.prepare_staging(&reintroduced).unwrap();
        fs::write(
            layout.staged_files(reintroduced.id()).join("object-0000"),
            b"game reintroduced",
        )
        .unwrap();
        TransactionManager::new(&layout, &NoFaults)
            .promote(
                "game-reintroduced",
                &reintroduced,
                &[Promotion {
                    path: RelativeGamePath::parse("Game.exe").unwrap(),
                    expected_md5: Md5::digest(b"game reintroduced").into(),
                }],
            )
            .unwrap();
        TransactionManager::new(&layout, &NoFaults)
            .prune_terminal(PruneAuditEvidence::FreshAndClear, &cleanup)
            .unwrap();

        let backup = layout.backup_root("ignored");
        let mut names = fs::read_dir(&backup)
            .unwrap()
            .filter_map(|entry| {
                let entry = entry.unwrap();
                (entry.file_name() != ".pd2launcher-retained").then(|| entry.file_name())
            })
            .collect::<Vec<_>>();
        names.sort();
        assert_eq!(names, ["object-0000", "object-0001"].map(OsString::from));
        assert!(
            names
                .iter()
                .all(|name| fs::metadata(backup.join(name)).unwrap().len() == 0)
        );

        let interrupted = manifest_for("Game.exe", b"game interrupted");
        layout.prepare_staging(&interrupted).unwrap();
        fs::write(
            layout.staged_files(interrupted.id()).join("object-0000"),
            b"game interrupted",
        )
        .unwrap();
        let fail = FailAt {
            target: 3,
            current: Cell::new(0),
        };
        assert!(
            TransactionManager::new(&layout, &fail)
                .promote(
                    "changing-rollback",
                    &interrupted,
                    &[Promotion {
                        path: RelativeGamePath::parse("Game.exe").unwrap(),
                        expected_md5: Md5::digest(b"game interrupted").into(),
                    }],
                )
                .is_err()
        );
        TransactionManager::new(&layout, &NoFaults)
            .repair()
            .unwrap();
        TransactionManager::new(&layout, &NoFaults)
            .prune_terminal(PruneAuditEvidence::FreshAndClear, &cleanup)
            .unwrap();
        let final_game = fs::read(temporary.path().join("Game.exe")).unwrap();
        assert!(final_game == b"game reintroduced" || final_game == b"game interrupted");
        assert!(
            names
                .iter()
                .all(|name| fs::metadata(backup.join(name)).unwrap().len() == 0)
        );

        let unknown_slot = backup.join("object-0002");
        fs::write(&unknown_slot, []).unwrap();
        let rejected = manifest_for("Game.exe", b"must not publish");
        layout.prepare_staging(&rejected).unwrap();
        fs::write(
            layout.staged_files(rejected.id()).join("object-0000"),
            b"must not publish",
        )
        .unwrap();
        assert!(matches!(
            TransactionManager::new(&layout, &NoFaults).promote(
                "unknown-backup-slot",
                &rejected,
                &[Promotion {
                    path: RelativeGamePath::parse("Game.exe").unwrap(),
                    expected_md5: Md5::digest(b"must not publish").into(),
                }],
            ),
            Err(TransactionError::Indeterminate)
        ));
        assert!(unknown_slot.exists());
        assert_ne!(
            fs::read(temporary.path().join("Game.exe")).unwrap(),
            b"must not publish"
        );
    }

    #[test]
    fn varying_staging_generations_keep_fixed_slots_bounded_and_probe_unknowns() {
        let temporary = TempDir::new().unwrap();
        let layout = UpdateLayout::new(temporary.path());
        let one = manifest_for("Game.exe", b"one");
        let two =
            manifest_for_many(&[("Game.exe", b"two game"), ("ProjectDiablo.dll", b"two dll")]);
        let mut stable_entries = None;

        for sequence in 0..64 {
            let manifest = if sequence % 2 == 0 { &two } else { &one };
            let lease = layout.acquire_exclusive().unwrap();
            layout
                .prepare_staging_with_lease("test-download", manifest, &lease)
                .unwrap();
            assert_eq!(
                UpdateTransactionProbe::new(layout.clone()).probe().unwrap(),
                TransactionEvidence::Repairable,
                "active unjournaled generation must be repairable"
            );
            for entry in manifest.entries() {
                let (object, partial) = lease
                    .staging_slots(manifest.id(), entry.path().as_str())
                    .unwrap();
                fs::write(layout.staged_files(manifest.id()).join(object), b"object").unwrap();
                fs::write(
                    layout.partial_files(manifest.id()).join(partial),
                    b"partial",
                )
                .unwrap();
            }
            layout
                .discard_unjournaled_staging(manifest, &lease)
                .unwrap();
            drop(lease);
            assert_eq!(
                UpdateTransactionProbe::new(layout.clone()).probe().unwrap(),
                TransactionEvidence::Clear
            );
            let entries = count_tree_entries(temporary.path());
            match stable_entries {
                None => stable_entries = Some(entries),
                Some(expected) => assert_eq!(entries, expected),
            }
            let staging = Directory::open(
                &layout
                    .metadata_root()
                    .join("staging")
                    .join(STAGING_CONTAINER),
            )
            .unwrap();
            reclaimed_staging_is_clean(&staging).unwrap();
            let retained_bytes = tree_bytes(&layout.metadata_root().join("staging"));
            assert!(retained_bytes < 10 * 1024 * 1024);
        }

        let staging_root = layout
            .metadata_root()
            .join("staging")
            .join(STAGING_CONTAINER);
        let unknown_object = staging_root.join("files/object-0002");
        let unknown_partial = staging_root.join(".partial/partial-0002");
        fs::write(&unknown_object, []).unwrap();
        fs::write(&unknown_partial, []).unwrap();
        assert_eq!(
            UpdateTransactionProbe::new(layout.clone()).probe().unwrap(),
            TransactionEvidence::Unsafe
        );
        assert!(layout.prepare_staging(&one).is_err());
        assert!(unknown_object.exists() && unknown_partial.exists());
        fs::remove_file(unknown_object).unwrap();
        fs::remove_file(unknown_partial).unwrap();

        let unknown = layout
            .metadata_root()
            .join("staging")
            .join(STAGING_CONTAINER)
            .join("manifest-shadow");
        fs::write(&unknown, b"foreign").unwrap();
        assert_eq!(
            UpdateTransactionProbe::new(layout.clone()).probe().unwrap(),
            TransactionEvidence::Unsafe
        );
        assert!(layout.prepare_staging(&one).is_err());
        assert_eq!(fs::read(unknown).unwrap(), b"foreign");
    }

    #[test]
    fn abandoned_download_intents_repair_after_pre_mid_and_post_body_restart_cuts() {
        for cut in 0..3 {
            let temporary = TempDir::new().unwrap();
            let layout = UpdateLayout::new(temporary.path());
            let manifest = manifest_for("Game.exe", b"restart bytes");
            let lease = layout.acquire_exclusive().unwrap();
            layout
                .prepare_staging_with_lease("killed-download", &manifest, &lease)
                .unwrap();
            if cut == 1 {
                fs::write(
                    layout.partial_files(manifest.id()).join("partial-0000"),
                    b"partial",
                )
                .unwrap();
            }
            if cut == 2 {
                fs::write(
                    layout.staged_files(manifest.id()).join("object-0000"),
                    b"restart bytes",
                )
                .unwrap();
                layout
                    .mark_staging_ready_for_journal("killed-download", &manifest, &lease)
                    .unwrap();
            }
            drop(lease);

            assert_eq!(
                UpdateTransactionProbe::new(layout.clone()).probe().unwrap(),
                TransactionEvidence::Repairable
            );
            assert_eq!(
                TransactionManager::new(&layout, &NoFaults)
                    .repair()
                    .unwrap(),
                RepairOutcome::Repaired(1)
            );
            assert_eq!(
                UpdateTransactionProbe::new(layout.clone()).probe().unwrap(),
                TransactionEvidence::Clear
            );
            assert_eq!(
                fs::metadata(layout.staged_files(manifest.id()).join("object-0000"))
                    .unwrap()
                    .len(),
                0
            );
            assert_eq!(
                fs::metadata(layout.partial_files(manifest.id()).join("partial-0000"))
                    .unwrap()
                    .len(),
                0
            );
        }
    }

    #[test]
    fn orphan_download_matrix_ignores_same_and_changed_manifest_pruned_journals() {
        for ready_for_journal in [false, true] {
            for same_manifest in [true, false] {
                let temporary = TempDir::new().unwrap();
                let layout = UpdateLayout::new(temporary.path());
                let old = manifest_for("Game.exe", b"old generation");
                layout.prepare_staging(&old).unwrap();
                fs::write(
                    layout.staged_files(old.id()).join("object-0000"),
                    b"old generation",
                )
                .unwrap();
                TransactionManager::new(&layout, &NoFaults)
                    .promote(
                        "already-pruned",
                        &old,
                        &[Promotion {
                            path: RelativeGamePath::parse("Game.exe").unwrap(),
                            expected_md5: Md5::digest(b"old generation").into(),
                        }],
                    )
                    .unwrap();
                TransactionManager::new(&layout, &NoFaults)
                    .prune_terminal(
                        PruneAuditEvidence::FreshAndClear,
                        &CleanupLease::acquire(&temporary.path().join("cleanup.lock")).unwrap(),
                    )
                    .unwrap();

                let changed = manifest_for("Game.exe", b"changed generation");
                let orphan = if same_manifest { &old } else { &changed };
                let lease = layout.acquire_exclusive().unwrap();
                layout
                    .prepare_staging_with_lease("orphan-ready", orphan, &lease)
                    .unwrap();
                fs::write(
                    layout.staged_files(orphan.id()).join("object-0000"),
                    b"uncommitted body",
                )
                .unwrap();
                if ready_for_journal {
                    layout
                        .mark_staging_ready_for_journal("orphan-ready", orphan, &lease)
                        .unwrap();
                }
                drop(lease);

                assert_eq!(
                    UpdateTransactionProbe::new(layout.clone()).probe().unwrap(),
                    TransactionEvidence::Repairable,
                    "orphan download intent must not bind to a pruned journal"
                );
                assert_eq!(
                    TransactionManager::new(&layout, &NoFaults)
                        .repair()
                        .unwrap(),
                    RepairOutcome::Repaired(1)
                );
                assert_eq!(
                    UpdateTransactionProbe::new(layout.clone()).probe().unwrap(),
                    TransactionEvidence::Clear
                );
                assert_eq!(
                    fs::metadata(layout.staged_files(orphan.id()).join("object-0000"))
                        .unwrap()
                        .len(),
                    0
                );
            }
        }
    }

    #[test]
    fn published_backup_before_identity_flag_is_repairable_and_repairs() {
        let temporary = TempDir::new().unwrap();
        let layout = UpdateLayout::new(temporary.path());
        let manifest = manifest_for("Game.exe", b"new game");
        layout.prepare_staging(&manifest).unwrap();
        fs::write(
            layout.staged_files(manifest.id()).join("object-0000"),
            b"new game",
        )
        .unwrap();
        fs::write(temporary.path().join("Game.exe"), b"old game").unwrap();
        let fail = FailAtFilesystemMutation {
            // Capacity slot creation, capacity sync, then the completed atomic
            // backup publication before its identity/flag journal write.
            target: 3,
            current: Cell::new(0),
        };
        assert!(
            TransactionManager::new(&layout, &fail)
                .promote(
                    "backup-published",
                    &manifest,
                    &[Promotion {
                        path: RelativeGamePath::parse("Game.exe").unwrap(),
                        expected_md5: Md5::digest(b"new game").into(),
                    }],
                )
                .is_err()
        );
        assert_eq!(
            UpdateTransactionProbe::new(layout.clone()).probe().unwrap(),
            TransactionEvidence::Repairable
        );
        assert_eq!(
            TransactionManager::new(&layout, &NoFaults)
                .repair()
                .unwrap(),
            RepairOutcome::Repaired(1)
        );
        assert_eq!(
            UpdateTransactionProbe::new(layout).probe().unwrap(),
            TransactionEvidence::Clear
        );
        assert_eq!(
            fs::read(temporary.path().join("Game.exe")).unwrap(),
            b"old game"
        );
    }

    #[test]
    fn repeated_rollbacks_of_distinct_new_paths_keep_removal_storage_bounded() {
        let temporary = TempDir::new().unwrap();
        let layout = UpdateLayout::new(temporary.path());
        let cleanup = CleanupLease::acquire(&temporary.path().join("cleanup.lock")).unwrap();

        for generation in 0..48 {
            let unique = format!("rollback-{generation:04}.bin");
            let manifest = manifest_for_many(&[
                (unique.as_str(), b"first promoted"),
                ("never-promoted.bin", b"second staged"),
            ]);
            layout.prepare_staging(&manifest).unwrap();
            fs::write(
                layout.staged_files(manifest.id()).join("object-0000"),
                b"first promoted",
            )
            .unwrap();
            fs::write(
                layout.staged_files(manifest.id()).join("object-0001"),
                b"second staged",
            )
            .unwrap();
            let fail = FailAtFilesystemMutation {
                // The first generation creates two capacity slots; later
                // generations reuse them. In both cases stop immediately after
                // the first newly-created project file is atomically promoted.
                target: if generation == 0 { 4 } else { 2 },
                current: Cell::new(0),
            };
            assert!(
                TransactionManager::new(&layout, &fail)
                    .promote(
                        &format!("rollback-{generation:04}"),
                        &manifest,
                        &[
                            Promotion {
                                path: RelativeGamePath::parse(&unique).unwrap(),
                                expected_md5: Md5::digest(b"first promoted").into(),
                            },
                            Promotion {
                                path: RelativeGamePath::parse("never-promoted.bin").unwrap(),
                                expected_md5: Md5::digest(b"second staged").into(),
                            },
                        ],
                    )
                    .is_err()
            );
            assert_eq!(
                UpdateTransactionProbe::new(layout.clone()).probe().unwrap(),
                TransactionEvidence::Repairable
            );
            TransactionManager::new(&layout, &NoFaults)
                .repair()
                .unwrap();
            assert!(!temporary.path().join(&unique).exists());
            assert!(!temporary.path().join("never-promoted.bin").exists());
            TransactionManager::new(&layout, &NoFaults)
                .prune_terminal(PruneAuditEvidence::FreshAndClear, &cleanup)
                .unwrap();

            // Removals leave nothing behind: no retained folder, no residue.
            assert!(
                !temporary
                    .path()
                    .join(crate::safe_fs::RETAINED_DIRECTORY)
                    .exists()
            );
            assert!(
                fs::read_dir(temporary.path())
                    .unwrap()
                    .flatten()
                    .all(|entry| { !crate::safe_fs::is_launcher_scratch(&entry.file_name()) })
            );
        }
    }

    #[test]
    fn torn_staging_growth_reconciles_only_contiguous_empty_slots() {
        let temporary = TempDir::new().unwrap();
        let layout = UpdateLayout::new(temporary.path());
        let one = manifest_for("Game.exe", b"one");
        layout.prepare_staging(&one).unwrap();
        let lease = layout.acquire_exclusive().unwrap();
        layout.discard_unjournaled_staging(&one, &lease).unwrap();
        let two = manifest_for_many(&[("Game.exe", b"two"), ("ProjectDiablo.dll", b"dll")]);
        let staging = lease
            .directories
            .staging
            .open_directory(Path::new(STAGING_CONTAINER))
            .unwrap();
        let previous = read_staging_state(&staging).unwrap();
        let growing = StagingState {
            schema: STAGING_STATE_SCHEMA,
            generation: previous.generation + 1,
            manifest_id: two.id().to_owned(),
            manifest_sha256: hex(&Sha256::digest(two.raw())),
            phase: StagingPhase::Growing {
                old_slots: 1,
                new_slots: 2,
            },
            slot_count: 2,
            slot_identities: previous.slot_identities,
            entries: two
                .entries()
                .iter()
                .enumerate()
                .map(|(index, entry)| StagingEntry {
                    path: entry.path().as_str().to_owned(),
                    md5: hex(&entry.digest().bytes()),
                    object_slot: numbered_name(STAGING_OBJECT_PREFIX, index),
                    partial_slot: numbered_name(STAGING_PARTIAL_PREFIX, index),
                })
                .collect(),
        };
        write_staging_state(&staging, &growing).unwrap();
        staging
            .open_directory(Path::new("files"))
            .unwrap()
            .open_or_create_file(OsStr::new("object-0001"))
            .unwrap();
        drop(lease);
        assert_eq!(
            UpdateTransactionProbe::new(layout.clone()).probe().unwrap(),
            TransactionEvidence::Repairable
        );
        TransactionManager::new(&layout, &NoFaults)
            .repair()
            .unwrap();
        assert_eq!(
            UpdateTransactionProbe::new(layout.clone()).probe().unwrap(),
            TransactionEvidence::Clear
        );
    }

    #[test]
    fn roll_forward_rejects_a_substituted_staged_manifest() {
        let temporary = TempDir::new().unwrap();
        let layout = UpdateLayout::new(temporary.path());
        let manifest = manifest_for("Game.exe", b"new game");
        layout.prepare_staging(&manifest).unwrap();
        fs::write(
            layout.staged_files(manifest.id()).join("object-0000"),
            b"new game",
        )
        .unwrap();
        let fail = FailAt {
            target: 8,
            current: Cell::new(0),
        };
        assert!(
            TransactionManager::new(&layout, &fail)
                .promote(
                    "manifest-substitution",
                    &manifest,
                    &[Promotion {
                        path: RelativeGamePath::parse("Game.exe").unwrap(),
                        expected_md5: Md5::digest(b"new game").into(),
                    }],
                )
                .is_err()
        );
        fs::write(
            layout.metadata_root().join("staging/active/manifest.json"),
            b"{\"checksum\":[\"00000000000000000000000000000000  Game.exe\"]}",
        )
        .unwrap();
        assert_eq!(
            UpdateTransactionProbe::new(layout.clone()).probe().unwrap(),
            TransactionEvidence::Unsafe
        );
        assert!(matches!(
            TransactionManager::new(&layout, &NoFaults).repair(),
            Err(TransactionError::Indeterminate)
        ));
        assert!(!layout.accepted_manifest().exists());
    }

    #[test]
    fn one_valid_staging_generation_map_repairs_a_missing_or_torn_peer() {
        let temporary = TempDir::new().unwrap();
        let layout = UpdateLayout::new(temporary.path());
        let manifest = manifest_for("Game.exe", b"generation");
        layout.prepare_staging(&manifest).unwrap();
        let staging = layout
            .metadata_root()
            .join("staging")
            .join(STAGING_CONTAINER);

        fs::write(staging.join(STAGING_MAP_A), b"torn").unwrap();
        layout.prepare_staging(&manifest).unwrap();
        let directory = Directory::open(&staging).unwrap();
        assert_eq!(
            read_staging_map(directory.open_file(Path::new(STAGING_MAP_A)).unwrap()).unwrap(),
            read_staging_map(directory.open_file(Path::new(STAGING_MAP_B)).unwrap()).unwrap()
        );

        fs::remove_file(staging.join(STAGING_MAP_B)).unwrap();
        layout.prepare_staging(&manifest).unwrap();
        assert!(staging.join(STAGING_MAP_A).is_file());
        assert!(staging.join(STAGING_MAP_B).is_file());
    }

    /// Every file a fresh copy with its own inode, as a Launcher folder copied
    /// to another device (or another place) is.
    fn copy_tree(from: &Path, to: &Path) {
        fs::create_dir_all(to).unwrap();
        for entry in fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            let target = to.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                copy_tree(&entry.path(), &target);
            } else {
                fs::copy(entry.path(), &target).unwrap();
            }
        }
    }

    /// Promotes `bytes` as `Game.exe` under `root` and prunes the journal,
    /// leaving an idle update namespace.
    fn update_and_prune(root: &Path, transaction_id: &str, bytes: &[u8], cleanup: &CleanupLease) {
        let layout = UpdateLayout::new(root);
        let manifest = manifest_for("Game.exe", bytes);
        layout.prepare_staging(&manifest).unwrap();
        fs::write(
            layout.staged_files(manifest.id()).join("object-0000"),
            bytes,
        )
        .unwrap();
        TransactionManager::new(&layout, &NoFaults)
            .promote(
                transaction_id,
                &manifest,
                &[Promotion {
                    path: RelativeGamePath::parse("Game.exe").unwrap(),
                    expected_md5: Md5::digest(bytes).into(),
                }],
            )
            .unwrap();
        assert_eq!(
            TransactionManager::new(&layout, &NoFaults)
                .prune_terminal(PruneAuditEvidence::FreshAndClear, cleanup)
                .unwrap(),
            1
        );
    }

    #[test]
    fn the_desktop_still_refuses_a_copied_idle_update_namespace() {
        let temporary = TempDir::new().unwrap();
        let cleanup = CleanupLease::acquire(&temporary.path().join("cleanup.lock")).unwrap();
        let original = temporary.path().join("original");
        fs::create_dir(&original).unwrap();
        fs::write(original.join("Game.exe"), b"old game").unwrap();
        update_and_prune(&original, "first", b"new game", &cleanup);
        assert_eq!(
            UpdateTransactionProbe::new(UpdateLayout::new(&original))
                .probe()
                .unwrap(),
            TransactionEvidence::Clear
        );

        let copied = temporary.path().join("copied");
        copy_tree(&original, &copied);
        assert_ne!(
            UpdateTransactionProbe::new(UpdateLayout::new(&copied))
                .probe()
                .unwrap(),
            TransactionEvidence::Clear,
            "the desktop compares every recorded slot identity"
        );
        let layout = UpdateLayout::new(&copied);
        assert!(
            layout
                .prepare_staging(&manifest_for("Game.exe", b"newer game"))
                .is_err()
        );
    }

    #[test]
    fn a_copied_idle_update_namespace_is_clear_and_takes_the_next_update() {
        ANDROID_HOST_FOR_TEST.with(|android| android.set(true));
        let temporary = TempDir::new().unwrap();
        let cleanup = CleanupLease::acquire(&temporary.path().join("cleanup.lock")).unwrap();
        let original = temporary.path().join("original");
        fs::create_dir(&original).unwrap();
        fs::write(original.join("Game.exe"), b"old game").unwrap();
        update_and_prune(&original, "first", b"new game", &cleanup);

        let copied = temporary.path().join("copied");
        copy_tree(&original, &copied);
        assert_eq!(
            UpdateTransactionProbe::new(UpdateLayout::new(&copied))
                .probe()
                .unwrap(),
            TransactionEvidence::Clear,
            "an idle namespace's empty slots vouch for nothing a copy changes"
        );
        update_and_prune(&copied, "second", b"newer game", &cleanup);
        assert_eq!(fs::read(copied.join("Game.exe")).unwrap(), b"newer game");
        assert_eq!(
            UpdateTransactionProbe::new(UpdateLayout::new(&copied))
                .probe()
                .unwrap(),
            TransactionEvidence::Clear
        );
    }

    fn count_tree_entries(root: &Path) -> usize {
        fs::read_dir(root)
            .unwrap()
            .map(|entry| {
                let path = entry.unwrap().path();
                1 + if path.is_dir() {
                    count_tree_entries(&path)
                } else {
                    0
                }
            })
            .sum()
    }

    fn tree_bytes(root: &Path) -> u64 {
        fs::read_dir(root)
            .unwrap()
            .map(|entry| {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    tree_bytes(&path)
                } else {
                    fs::metadata(path).unwrap().len()
                }
            })
            .sum()
    }

    #[test]
    fn live_export_cleanup_lease_blocks_pruning_for_the_entire_lifetime() {
        let temporary = TempDir::new().unwrap();
        let layout = UpdateLayout::new(temporary.path());
        let manifest = manifest_for("Game.exe", b"new game");
        layout.prepare_staging(&manifest).unwrap();
        fs::write(
            layout.staged_files(manifest.id()).join("object-0000"),
            b"new game",
        )
        .unwrap();
        TransactionManager::new(&layout, &NoFaults)
            .promote(
                "export-race",
                &manifest,
                &[Promotion {
                    path: RelativeGamePath::parse("Game.exe").unwrap(),
                    expected_md5: Md5::digest(b"new game").into(),
                }],
            )
            .unwrap();
        let cleanup_path = temporary.path().join("cleanup.lock");
        let export_guard = CleanupLease::acquire(&cleanup_path).unwrap();
        let project_root = temporary.path().to_owned();
        let journal = layout.journal_path("export-race");
        let (started_tx, started_rx) = mpsc::channel();
        let (finished_tx, finished_rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            started_tx.send(()).unwrap();
            let cleanup = CleanupLease::acquire(&cleanup_path).unwrap();
            let layout = UpdateLayout::new(project_root);
            let result = TransactionManager::new(&layout, &NoFaults)
                .prune_terminal(PruneAuditEvidence::FreshAndClear, &cleanup);
            finished_tx.send(result).unwrap();
        });

        started_rx.recv().unwrap();
        assert!(finished_rx.recv_timeout(Duration::from_millis(50)).is_err());
        assert!(journal.is_file());
        drop(export_guard);
        assert_eq!(
            finished_rx
                .recv_timeout(Duration::from_secs(1))
                .unwrap()
                .unwrap(),
            1
        );
        worker.join().unwrap();
        assert!(journal.is_file());
    }

    #[test]
    fn clear_audit_pruning_is_a_read_only_no_op_before_transaction_layout_exists() {
        let temporary = TempDir::new().unwrap();
        let project = temporary.path().join("ProjectD2");
        let layout = UpdateLayout::new(&project);
        let cleanup = CleanupLease::acquire(&temporary.path().join("cleanup.lock")).unwrap();

        assert_eq!(
            TransactionManager::new(&layout, &NoFaults)
                .prune_terminal(PruneAuditEvidence::FreshAndClear, &cleanup)
                .unwrap(),
            0
        );
        assert!(!project.exists());

        fs::create_dir(&project).unwrap();
        assert_eq!(
            TransactionManager::new(&layout, &NoFaults)
                .prune_terminal(PruneAuditEvidence::FreshAndClear, &cleanup)
                .unwrap(),
            0
        );
        assert!(fs::read_dir(&project).unwrap().next().is_none());
    }

    #[test]
    fn terminal_pruning_rejects_link_substitution_and_retains_the_journal() {
        for hardlink in [false, true] {
            let temporary = TempDir::new().unwrap();
            let layout = UpdateLayout::new(temporary.path());
            let manifest = manifest_for("Game.exe", b"new game");
            layout.prepare_staging(&manifest).unwrap();
            fs::write(
                layout.staged_files(manifest.id()).join("object-0000"),
                b"new game",
            )
            .unwrap();
            fs::write(temporary.path().join("Game.exe"), b"old game").unwrap();
            let transaction_id = if hardlink {
                "prune-hardlink"
            } else {
                "prune-symlink"
            };
            TransactionManager::new(&layout, &NoFaults)
                .promote(
                    transaction_id,
                    &manifest,
                    &[Promotion {
                        path: RelativeGamePath::parse("Game.exe").unwrap(),
                        expected_md5: Md5::digest(b"new game").into(),
                    }],
                )
                .unwrap();
            let backup_file = layout.backup_root(transaction_id).join("object-0000");
            let outside = temporary.path().join("outside");
            if hardlink {
                fs::hard_link(&backup_file, &outside).unwrap();
            } else {
                fs::remove_file(&backup_file).unwrap();
                fs::write(&outside, b"outside").unwrap();
                symlink(&outside, &backup_file).unwrap();
            }

            assert!(matches!(
                TransactionManager::new(&layout, &NoFaults).prune_terminal(
                    PruneAuditEvidence::FreshAndClear,
                    &CleanupLease::acquire(&temporary.path().join("cleanup.lock")).unwrap(),
                ),
                Err(TransactionError::UnsafePath)
            ));
            assert!(layout.journal_path(transaction_id).is_file());
        }
    }

    fn manifest_for(path: &str, bytes: &[u8]) -> Pd2Manifest {
        manifest_for_many(&[(path, bytes)])
    }

    fn manifest_for_many(entries: &[(&str, &[u8])]) -> Pd2Manifest {
        let body = serde_json::to_vec(&serde_json::json!({
            "checksum": entries
                .iter()
                .map(|(path, bytes)| format!("{}  {path}", hex(&Md5::digest(bytes))))
                .collect::<Vec<_>>()
        }))
        .unwrap();
        Pd2Manifest::parse(ManifestResponse {
            requested_url: PD2_MANIFEST_URL.to_owned(),
            final_url: PD2_MANIFEST_URL.to_owned(),
            declared_length: Some(body.len() as u64),
            body,
        })
        .unwrap()
    }

    #[test]
    fn legacy_journal_backup_identities_are_rebound_onto_the_transactions_filesystem() {
        let mut legacy = grammar_journal(JournalState::Planned);
        legacy.backup_identities = vec![
            FileIdentity::LegacyDevice {
                device: 53,
                inode: 10,
            },
            FileIdentity::LegacyDevice {
                device: 53,
                inode: 11,
            },
        ];
        let bytes = serde_json::to_vec(&legacy).unwrap();
        assert!(
            String::from_utf8_lossy(&bytes).contains(r#"{"device":53,"inode":10}"#),
            "fixture uses the schema-2 identity encoding"
        );

        let parsed = parse_journal_bytes(&bytes, 99).unwrap();
        assert_eq!(
            parsed.backup_identities,
            [
                FileIdentity::for_test(99, 10),
                FileIdentity::for_test(99, 11)
            ]
        );
        assert_eq!(
            parsed,
            Journal {
                backup_identities: parsed.backup_identities.clone(),
                ..legacy.clone()
            }
        );

        let mut mixed = legacy;
        mixed.backup_identities[1] = FileIdentity::for_test(99, 11);
        assert!(matches!(
            parse_journal_bytes(&serde_json::to_vec(&mixed).unwrap(), 99),
            Err(TransactionError::InvalidJournal)
        ));
    }
}
