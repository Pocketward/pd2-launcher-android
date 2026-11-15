//! The interrupted-stage marker.
//!
//! Before a stage that changes the Managed prefix in a way its own evidence
//! cannot resume (creating a new prefix, running a Vendor installer, or
//! importing a game), the installer records that stage in
//! `<root>/.pd2launcher-stage.json` while it holds the installation lease.
//! When the stage's step returned without an error, the installer marks the
//! record finished; it clears the marker only after a fresh audit proved
//! the stage's evidence. An audit that finds the marker beside evidence the
//! stage left part way reports `StageInterrupted`, which only a confirmed
//! Start over resolves, instead of `Indeterminate`.
//!
//! On the Android host Start over itself writes a started base-game record
//! before its first removal and clears it after its last one
//! (`install::start_over_android`): a Start over that a stop left part way
//! then reads as an interrupted step while a file of the game is there, and
//! the player's next Start over finishes it; it never reads as a Ready
//! installation.
//!
//! The games are proven by their files alone, and a file can be
//! there but short while it is written. So for a game stage that is not
//! finished, any file it wrote means the stage stopped part way; a
//! finished one is judged by its files.

use std::{
    ffi::OsStr,
    io::{self, Read},
    path::Path,
};

use serde::{Deserialize, Serialize};

use crate::{InterruptedStage, safe_fs::Directory};

pub(crate) const STAGE_MARKER: &str = ".pd2launcher-stage.json";
const SCHEMA: u32 = 1;
const LIMIT: u64 = 1024;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum StageName {
    Prefix,
    BaseGame,
    Expansion,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Record {
    schema: u32,
    stage: StageName,
    /// The stage's step returned without an error. Left out while it runs,
    /// so such a record is the same as one an earlier release wrote.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    finished: bool,
}

/// What the managed root says about an interrupted stage.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MarkerState {
    Absent,
    /// The stage started and its step has not returned.
    Present(InterruptedStage),
    /// The stage's step returned; only the proof and the clear are left.
    Finished(InterruptedStage),
    /// Present but not a marker the Launcher wrote: fails closed.
    Invalid,
}

pub(crate) fn read(root: &Directory) -> MarkerState {
    let file = match root.open_file(Path::new(STAGE_MARKER)) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return MarkerState::Absent,
        Err(_) => return MarkerState::Invalid,
    };
    let mut bytes = Vec::new();
    if file.take(LIMIT + 1).read_to_end(&mut bytes).is_err() || bytes.len() as u64 > LIMIT {
        return MarkerState::Invalid;
    }
    match serde_json::from_slice::<Record>(&bytes) {
        Ok(record) if record.schema == SCHEMA => {
            let stage = match record.stage {
                StageName::Prefix => InterruptedStage::Prefix,
                StageName::BaseGame => InterruptedStage::BaseGame,
                StageName::Expansion => InterruptedStage::Expansion,
            };
            if record.finished {
                MarkerState::Finished(stage)
            } else {
                MarkerState::Present(stage)
            }
        }
        _ => MarkerState::Invalid,
    }
}

/// Record `stage` durably, as started. The caller holds the installation
/// lease.
pub(crate) fn write(root: &Directory, stage: InterruptedStage) -> io::Result<()> {
    write_record(root, stage, false)
}

/// Record that `stage`'s step returned without an error, before its
/// evidence is audited. The caller holds the installation lease.
pub(crate) fn finish(root: &Directory, stage: InterruptedStage) -> io::Result<()> {
    write_record(root, stage, true)
}

fn write_record(root: &Directory, stage: InterruptedStage, finished: bool) -> io::Result<()> {
    let record = Record {
        schema: SCHEMA,
        stage: match stage {
            InterruptedStage::Prefix => StageName::Prefix,
            InterruptedStage::BaseGame => StageName::BaseGame,
            InterruptedStage::Expansion => StageName::Expansion,
        },
        finished,
    };
    let bytes = serde_json::to_vec(&record)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "stage marker encode failed"))?;
    root.atomic_write(OsStr::new(STAGE_MARKER), &bytes)
}

/// Remove the marker once the stage's evidence was proven, or when a
/// confirmed Start over removed what the stage left. The caller holds the
/// installation lease.
pub(crate) fn clear(root: &Directory) -> io::Result<()> {
    match root.open_file(Path::new(STAGE_MARKER)) {
        Ok(file) => root.remove_open_file_bounded(OsStr::new(STAGE_MARKER), &file),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn a_marker_round_trips_and_clears_and_anything_else_fails_closed() {
        let fixture = tempfile::tempdir().unwrap();
        let root = Directory::open(fixture.path()).unwrap();
        assert_eq!(read(&root), MarkerState::Absent);
        clear(&root).expect("clearing an absent marker is a no-op");
        for stage in [
            InterruptedStage::Prefix,
            InterruptedStage::BaseGame,
            InterruptedStage::Expansion,
        ] {
            write(&root, stage).unwrap();
            assert_eq!(read(&root), MarkerState::Present(stage));
            finish(&root, stage).unwrap();
            assert_eq!(read(&root), MarkerState::Finished(stage));
        }
        // A started record is written as earlier releases wrote it.
        write(&root, InterruptedStage::BaseGame).unwrap();
        assert_eq!(
            fs::read(fixture.path().join(STAGE_MARKER)).unwrap(),
            br#"{"schema":1,"stage":"base-game"}"#
        );
        clear(&root).unwrap();
        assert_eq!(read(&root), MarkerState::Absent);

        for bytes in [
            &b"{}"[..],
            br#"{"schema":2,"stage":"prefix"}"#,
            br#"{"schema":1,"stage":"patch"}"#,
            br#"{"schema":1,"stage":"prefix","extra":1}"#,
            br#"{"schema":1,"stage":"prefix","finished":"yes"}"#,
        ] {
            fs::write(fixture.path().join(STAGE_MARKER), bytes).unwrap();
            assert_eq!(read(&root), MarkerState::Invalid);
        }
        fs::remove_file(fixture.path().join(STAGE_MARKER)).unwrap();
        std::os::unix::fs::symlink("/etc/hostname", fixture.path().join(STAGE_MARKER)).unwrap();
        assert_eq!(read(&root), MarkerState::Invalid);
    }
}
