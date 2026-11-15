//! Pure launcher state and policy.

use std::{error::Error, fmt};

/// The managed installation state established by an audit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InstallationStatus {
    NeedsBaseGame,
    NeedsExpansion,
    NeedsClient,
    /// The game is complete, but a Launcher-installed runtime or prefix
    /// component (such as Wine Mono or the .NET Desktop Runtime) is missing
    /// or must be replaced. Continuing installs only that component.
    NeedsRuntime,
    RepairRequired,
    Indeterminate,
    /// A Launcher stage that changes the prefix was interrupted before its
    /// evidence was complete, as recorded by the stage marker. Only a
    /// confirmed Start over continues from here.
    StageInterrupted(InterruptedStage),
    Ready,
}

/// The stage an interrupted-stage marker names.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InterruptedStage {
    /// Creating a new Managed prefix (wineboot, Gecko, Mono, .NET).
    Prefix,
    BaseGame,
    Expansion,
}

/// The last known result of the advisory update check.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UpdateStatus {
    Unknown,
    Checking,
    UpToDate,
    Available,
    CheckFailed,
}

/// The result of the most recent launch preflight.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LaunchPreflight {
    Passed,
    Failed,
}

/// A normalized, portable path relative to the managed installation root.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RelativeGamePath(String);

impl RelativeGamePath {
    pub fn parse(path: &str) -> Result<Self, InvalidRelativeGamePath> {
        let invalid_character = |character: char| {
            character.is_control()
                || matches!(character, '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
        };
        let invalid_component = |component: &str| {
            component.is_empty()
                || matches!(component, "." | "..")
                || component.chars().any(invalid_character)
        };

        if path.split('/').any(invalid_component) {
            return Err(InvalidRelativeGamePath);
        }

        Ok(Self(path.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Returned when a path cannot safely address a file below the managed root.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidRelativeGamePath;

impl fmt::Display for InvalidRelativeGamePath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("path must be a portable path relative to the managed root")
    }
}

impl Error for InvalidRelativeGamePath {}
