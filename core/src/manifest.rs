//! Strict PD2 manifest parsing and portable path validation.

use std::{collections::BTreeMap, error::Error, fmt};

use serde_json::Value;
use sha2::{Digest as _, Sha256};

use crate::{
    RelativeGamePath,
    audit::{AcceptedClientManifest, ClientFileExpectation, Md5Digest},
};

pub(crate) const PD2_ORIGIN: &str = "https://pd2-client-files.projectdiablo2.com";
pub(crate) const PD2_MANIFEST_URL: &str =
    "https://pd2-client-files.projectdiablo2.com/metadata.json";
const MAX_MANIFEST_BYTES: usize = 1024 * 1024;
const MANIFEST_NAME: &str = "metadata.json";

/// Where PD2 client files come from. The built-in origin keeps its exact-origin
/// policy; a user-supplied custom base skips the origin allow-list
/// and permits redirects, while every object is still checked against the MD5
/// in the manifest that base serves.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) enum Pd2ClientBase {
    #[default]
    BuiltIn,
    /// Normalized `http`/`https` base without a trailing slash.
    Custom(String),
}

impl Pd2ClientBase {
    pub(crate) fn manifest_url(&self) -> String {
        match self {
            Self::BuiltIn => PD2_MANIFEST_URL.to_owned(),
            Self::Custom(base) => format!("{base}/{MANIFEST_NAME}"),
        }
    }

    fn origin(&self) -> &str {
        match self {
            Self::BuiltIn => PD2_ORIGIN,
            Self::Custom(base) => base,
        }
    }

    pub(crate) fn is_custom(&self) -> bool {
        matches!(self, Self::Custom(_))
    }
}
const MAX_MANIFEST_ENTRIES: usize = 4096;
const MAX_FILENAME_BYTES: usize = 255;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ManifestEntry {
    path: RelativeGamePath,
    digest: Md5Digest,
    object_url: String,
}

impl ManifestEntry {
    pub(crate) fn path(&self) -> &RelativeGamePath {
        &self.path
    }

    pub(crate) fn digest(&self) -> Md5Digest {
        self.digest
    }

    pub(crate) fn object_url(&self) -> &str {
        &self.object_url
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Pd2Manifest {
    id: String,
    raw: Vec<u8>,
    entries: Vec<ManifestEntry>,
    custom_source: bool,
}

impl Pd2Manifest {
    pub(crate) fn parse(response: ManifestResponse) -> Result<Self, ManifestError> {
        Self::parse_from(response, &Pd2ClientBase::BuiltIn)
    }

    pub(crate) fn parse_from(
        response: ManifestResponse,
        base: &Pd2ClientBase,
    ) -> Result<Self, ManifestError> {
        response.validate(base)?;
        let raw = response.body;
        let json = raw.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(&raw);
        let value: Value = serde_json::from_slice(json).map_err(|_| ManifestError::InvalidJson)?;
        let object = value.as_object().ok_or(ManifestError::InvalidSchema)?;
        if object.len() != 1 {
            return Err(ManifestError::InvalidSchema);
        }
        let checksums = object
            .get("checksum")
            .and_then(Value::as_array)
            .ok_or(ManifestError::InvalidSchema)?;
        if checksums.is_empty() || checksums.len() > MAX_MANIFEST_ENTRIES {
            return Err(ManifestError::EntryCount);
        }

        let mut entries = Vec::with_capacity(checksums.len());
        let mut exact = BTreeMap::new();
        let mut folded = BTreeMap::new();
        for checksum in checksums {
            let line = checksum.as_str().ok_or(ManifestError::InvalidEntry)?;
            let (digest, filename) = parse_checksum_line(line)?;
            let path = validate_flat_filename(filename)?;
            if exact.insert(path.clone(), ()).is_some() {
                return Err(ManifestError::DuplicatePath);
            }
            let folded_name = filename.to_ascii_lowercase();
            if folded.insert(folded_name, filename).is_some() {
                return Err(ManifestError::CaseCollision);
            }
            entries.push(ManifestEntry {
                path,
                digest,
                object_url: format!("{}/{}", base.origin(), encode_path_segment(filename)),
            });
        }

        let id = hex(&Sha256::digest(&raw));
        Ok(Self {
            id,
            raw,
            entries,
            custom_source: base.is_custom(),
        })
    }

    /// Objects of a manifest from a custom base may be redirected.
    pub(crate) fn is_custom_source(&self) -> bool {
        self.custom_source
    }

    pub(crate) fn id(&self) -> &str {
        &self.id
    }

    pub(crate) fn raw(&self) -> &[u8] {
        &self.raw
    }

    pub(crate) fn entries(&self) -> &[ManifestEntry] {
        &self.entries
    }

    pub(crate) fn accepted(&self) -> Result<AcceptedClientManifest, ManifestError> {
        AcceptedClientManifest::new(
            self.entries
                .iter()
                .map(|entry| ClientFileExpectation::new(entry.path.clone(), entry.digest)),
        )
        .map_err(|_| ManifestError::MissingLaunchFiles)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ManifestResponse {
    pub(crate) requested_url: String,
    pub(crate) final_url: String,
    pub(crate) declared_length: Option<u64>,
    pub(crate) body: Vec<u8>,
}

impl ManifestResponse {
    fn validate(&self, base: &Pd2ClientBase) -> Result<(), ManifestError> {
        let origin_accepted = match base {
            Pd2ClientBase::BuiltIn => {
                self.requested_url == PD2_MANIFEST_URL && self.final_url == PD2_MANIFEST_URL
            }
            Pd2ClientBase::Custom(_) => self.requested_url == base.manifest_url(),
        };
        if !origin_accepted {
            return Err(ManifestError::OriginRejected);
        }
        if self.body.len() > MAX_MANIFEST_BYTES
            || self
                .declared_length
                .is_some_and(|length| length > MAX_MANIFEST_BYTES as u64)
        {
            return Err(ManifestError::BodyTooLarge);
        }
        if self
            .declared_length
            .is_some_and(|length| length != self.body.len() as u64)
        {
            return Err(ManifestError::Truncated);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ManifestError {
    OriginRejected,
    BodyTooLarge,
    Truncated,
    InvalidJson,
    InvalidSchema,
    EntryCount,
    InvalidEntry,
    UnsafePath,
    DuplicatePath,
    CaseCollision,
    MissingLaunchFiles,
}

impl fmt::Display for ManifestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "PD2 manifest rejected: {self:?}")
    }
}

impl Error for ManifestError {}

fn parse_checksum_line(line: &str) -> Result<(Md5Digest, &str), ManifestError> {
    if line.len() < 35 || line.as_bytes().get(32..34) != Some(b"  ") {
        return Err(ManifestError::InvalidEntry);
    }
    let digest = line.get(..32).ok_or(ManifestError::InvalidEntry)?;
    if !digest
        .bytes()
        .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(ManifestError::InvalidEntry);
    }
    let digest = Md5Digest::parse_hex(digest).map_err(|_| ManifestError::InvalidEntry)?;
    Ok((digest, &line[34..]))
}

fn validate_flat_filename(filename: &str) -> Result<RelativeGamePath, ManifestError> {
    if filename.is_empty()
        || filename.len() > MAX_FILENAME_BYTES
        || !filename.is_ascii()
        || filename.contains(['/', '\\'])
        || filename == "."
        || filename == ".."
    {
        return Err(ManifestError::UnsafePath);
    }
    RelativeGamePath::parse(filename).map_err(|_| ManifestError::UnsafePath)
}

fn encode_path_segment(filename: &str) -> String {
    let mut encoded = String::with_capacity(filename.len());
    for byte in filename.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push_str(&format!("{byte:02x}"));
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(body: &[u8]) -> ManifestResponse {
        ManifestResponse {
            requested_url: PD2_MANIFEST_URL.to_owned(),
            final_url: PD2_MANIFEST_URL.to_owned(),
            declared_length: Some(body.len() as u64),
            body: body.to_vec(),
        }
    }

    #[test]
    fn parses_the_reviewed_bom_contract_and_constructs_approved_object_urls() {
        let body = b"\xef\xbb\xbf{\"checksum\":[\"d41d8cd98f00b204e9800998ecf8427e  Game.exe\",\"d41d8cd98f00b204e9800998ecf8427e  ProjectDiablo.dll\",\"d41d8cd98f00b204e9800998ecf8427e  Diablo II.exe\"]}";

        let manifest = Pd2Manifest::parse(response(body)).expect("reviewed contract");

        assert_eq!(manifest.entries().len(), 3);
        assert_eq!(
            manifest.entries()[2].object_url(),
            "https://pd2-client-files.projectdiablo2.com/Diablo%20II.exe"
        );
        assert!(manifest.accepted().is_ok());
    }

    #[test]
    fn a_custom_base_serves_manifest_and_objects_without_the_origin_allow_list() {
        let body = b"{\"checksum\":[\"d41d8cd98f00b204e9800998ecf8427e  Game.exe\",\"d41d8cd98f00b204e9800998ecf8427e  ProjectDiablo.dll\"]}";
        let base = Pd2ClientBase::Custom("https://mirror.example/pd2".to_owned());
        assert_eq!(
            base.manifest_url(),
            "https://mirror.example/pd2/metadata.json"
        );
        let manifest = Pd2Manifest::parse_from(
            ManifestResponse {
                requested_url: base.manifest_url(),
                // A custom base may redirect, e.g. to a CDN.
                final_url: "https://cdn.mirror.example/x/metadata.json".to_owned(),
                declared_length: Some(body.len() as u64),
                body: body.to_vec(),
            },
            &base,
        )
        .expect("custom manifest");
        assert!(manifest.is_custom_source());
        assert_eq!(
            manifest.entries()[0].object_url(),
            "https://mirror.example/pd2/Game.exe"
        );
        // The manifest's own MD5 entries still become the accepted identity.
        assert!(manifest.accepted().is_ok());

        // The custom base does not accept a manifest requested elsewhere, and
        // the built-in base keeps rejecting every non-approved origin.
        let mut elsewhere = response(body);
        elsewhere.requested_url = "https://other.example/metadata.json".to_owned();
        assert_eq!(
            Pd2Manifest::parse_from(elsewhere, &base).unwrap_err(),
            ManifestError::OriginRejected
        );
        let mut redirected = response(body);
        redirected.final_url = "https://cdn.mirror.example/metadata.json".to_owned();
        assert_eq!(
            Pd2Manifest::parse(redirected).unwrap_err(),
            ManifestError::OriginRejected
        );
        assert!(
            !Pd2Manifest::parse(response(body))
                .unwrap()
                .is_custom_source()
        );
    }

    #[test]
    fn rejects_wrong_origins_redirects_lengths_and_oversized_bodies() {
        let valid = b"{\"checksum\":[\"d41d8cd98f00b204e9800998ecf8427e  Game.exe\"]}";
        let mut redirected = response(valid);
        redirected.final_url = "https://example.test/metadata.json".to_owned();
        assert_eq!(
            Pd2Manifest::parse(redirected).unwrap_err(),
            ManifestError::OriginRejected
        );

        let mut truncated = response(valid);
        truncated.declared_length = Some(valid.len() as u64 + 1);
        assert_eq!(
            Pd2Manifest::parse(truncated).unwrap_err(),
            ManifestError::Truncated
        );

        let too_large = vec![b' '; MAX_MANIFEST_BYTES + 1];
        assert_eq!(
            Pd2Manifest::parse(response(&too_large)).unwrap_err(),
            ManifestError::BodyTooLarge
        );
    }

    #[test]
    fn rejects_loose_grammar_unsafe_paths_and_case_collisions() {
        for line in [
            "D41D8CD98F00B204E9800998ECF8427E  Game.exe",
            "d41d8cd98f00b204e9800998ecf8427e Game.exe",
            "d41d8cd98f00b204e9800998ecf8427e  ../Game.exe",
            "d41d8cd98f00b204e9800998ecf8427e  nested/Game.exe",
            "d41d8cd98f00b204e9800998ecf8427e  G\u{e1}me.exe",
            "\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}  Game.exe",
        ] {
            let body = format!("{{\"checksum\":[{line:?}]}}");
            assert!(
                Pd2Manifest::parse(response(body.as_bytes())).is_err(),
                "{line}"
            );
        }

        let collision = b"{\"checksum\":[\"d41d8cd98f00b204e9800998ecf8427e  Game.exe\",\"d41d8cd98f00b204e9800998ecf8427e  game.exe\"]}";
        assert_eq!(
            Pd2Manifest::parse(response(collision)).unwrap_err(),
            ManifestError::CaseCollision
        );

        let too_many = serde_json::to_vec(&serde_json::json!({
            "checksum": vec!["d41d8cd98f00b204e9800998ecf8427e  Game.exe"; MAX_MANIFEST_ENTRIES + 1]
        }))
        .unwrap();
        assert_eq!(
            Pd2Manifest::parse(response(&too_many)).unwrap_err(),
            ManifestError::EntryCount
        );
    }

    #[test]
    fn arbitrary_manifest_entries_never_produce_a_nested_or_absolute_path() {
        for first in 0_u8..=255 {
            let filename = String::from_utf8_lossy(&[first, b'.', b'x']).into_owned();
            let body =
                format!("{{\"checksum\":[\"d41d8cd98f00b204e9800998ecf8427e  {filename}\"]}}");
            if let Ok(manifest) = Pd2Manifest::parse(response(body.as_bytes())) {
                let path = manifest.entries()[0].path().as_str();
                assert!(!path.contains(['/', '\\']));
                assert!(!path.starts_with('/'));
            }
        }
    }
}
