//! The direct download install source ([`crate::blizzard`]): Blizzard's
//! complete installers fetched piece by piece into the artifact cache from
//! the torrent each verified wrapper carries, without Blizzard's downloader
//! and without Wine, then checked and kept as any complete installer.

use rustix::fs::FallocateFlags;
use zeroize::Zeroize;

use super::*;
use crate::blizzard::{self, AccessCookie, Download, Outcome, PayloadFile, ProductPin, Torrent};

/// An access cookie that expires this soon is not started with: a fresh
/// wrapper is downloaded first.
const COOKIE_MARGIN: Duration = Duration::from_secs(60 * 60);
/// The most of a wrapper read, its acquisition's own bound.
const WRAPPER_LIMIT: u64 = 8_000_000;

/// The cache folder the direct download fills for a product, kept between
/// attempts until its files check; then it becomes the product's complete
/// installer folder.
fn staging_name(identity: &CompletePayloadIdentity) -> String {
    format!("{}.partial", identity.cache_directory)
}

fn pin(kind: ArtifactKind) -> Option<ProductPin> {
    match kind {
        ArtifactKind::BaseGameWrapper => Some(blizzard::BASE_GAME),
        ArtifactKind::ExpansionWrapper => Some(blizzard::EXPANSION),
        _ => None,
    }
}

/// One product ready to fetch.
struct Prepared {
    kind: ArtifactKind,
    identity: CompletePayloadIdentity,
    pin: ProductPin,
    torrent: Torrent,
    cookie: AccessCookie,
    staging: Directory,
    files: Vec<Option<PayloadFile>>,
}

/// Fetch the complete installers of `wrappers`' products at the same time
/// and keep each that checks; the failures, by product.
pub(super) fn download_payloads(
    paths: &ManagedPaths,
    wrappers: &[&VerifiedArtifact],
    control: &OperationControl,
) -> Vec<(ArtifactKind, VendorFailure)> {
    let Ok(cache) = paths
        .directory()
        .open_directory(Path::new("cache/artifacts"))
    else {
        return wrappers
            .iter()
            .map(|wrapper| (wrapper.kind(), VendorFailure::Install))
            .collect();
    };
    let mut failures = Vec::new();
    let mut prepared = Vec::new();
    for wrapper in wrappers {
        match prepare(&cache, wrapper, SystemTime::now()) {
            Ok(product) => prepared.push(product),
            Err(failure) => failures.push((wrapper.kind(), failure)),
        }
    }
    if prepared.is_empty() {
        return failures;
    }
    let phase = if prepared
        .iter()
        .any(|product| product.kind == ArtifactKind::BaseGameWrapper)
    {
        WorkPhase::BaseGameDownload
    } else {
        WorkPhase::ExpansionDownload
    };
    let limits = blizzard::Limits::STANDARD;
    let transport = blizzard::HttpTransport::new(limits.idle);
    let downloads = prepared
        .iter()
        .map(|product| Download {
            torrent: &product.torrent,
            cookie: &product.cookie,
            base: product.pin.base,
            files: &product.files,
        })
        .collect::<Vec<_>>();
    let fetched = blizzard::fetch(&downloads, &transport, &limits, control, &|progress| {
        control.report(
            WorkProgress::for_phase(phase)
                .with_file_bytes(progress.done, Some(progress.total))
                .with_verification(0, Some(1)),
        );
    });
    drop(downloads);
    let summary = match fetched {
        Ok(summary) => summary,
        Err(blizzard::Cancelled) => {
            // Every piece written is kept for the next attempt.
            if prepared.iter().all(|product| synced(&product.files)) {
                control.mark_cancelled_safe();
            } else {
                control.mark_cancel_unproven();
            }
            failures.extend(
                prepared
                    .iter()
                    .map(|product| (product.kind, VendorFailure::Install)),
            );
            return failures;
        }
    };
    let megabytes = |bytes: u64| bytes as f64 / 1_000_000.0;
    let seconds = summary.elapsed.as_secs_f64().max(0.001);
    eprintln!(
        "pd2-launcher: direct download: {:.1} MB fetched and {:.1} MB already on disk in {seconds:.1} s ({:.1} MB/s), up to {} connections: {:?}",
        megabytes(summary.fetched),
        megabytes(summary.resumed),
        megabytes(summary.fetched) / seconds,
        summary.peak_connections,
        summary.outcomes
    );
    // Each finished payload is checked as any complete installer, all at
    // the same time.
    let finished = thread::scope(|scope| {
        prepared
            .into_iter()
            .zip(summary.outcomes)
            .map(|(product, outcome)| {
                let kind = product.kind;
                let cache = &cache;
                (
                    kind,
                    scope.spawn(move || finish(cache, product, outcome, control)),
                )
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|(kind, finishing)| {
                (
                    kind,
                    finishing.join().unwrap_or(Err(VendorFailure::Install)),
                )
            })
            .collect::<Vec<_>>()
    });
    if control.is_cancelled() {
        // Stopped during the check: every piece is synced and kept.
        control.mark_cancelled_safe();
    }
    failures.extend(
        finished
            .into_iter()
            .filter_map(|(kind, result)| result.err().map(|failure| (kind, failure))),
    );
    failures
}

/// Read the wrapper's torrent and cookie, check them against the pins, and
/// open the product's files, made at full length.
fn prepare(
    cache: &Directory,
    wrapper: &VerifiedArtifact,
    now: SystemTime,
) -> Result<Prepared, VendorFailure> {
    let kind = wrapper.kind();
    let identity = complete_payload_identity(kind).ok_or(VendorFailure::Install)?;
    let pin = pin(kind).ok_or(VendorFailure::Install)?;
    let mismatch = |file: &str, check: &str| {
        crate::activity::record_payload_problem(identity.source_directory, file, check, None);
        VendorFailure::DirectDownload(DirectFailure::Mismatch)
    };
    let mut stub = Vec::new();
    open_verified_program(wrapper)
        .map_err(|_| VendorFailure::Install)?
        .take(WRAPPER_LIMIT + 1)
        .read_to_end(&mut stub)
        .map_err(|_| VendorFailure::Install)?;
    let torrent = Torrent::from_stub(&stub);
    let cookie = AccessCookie::find(&stub);
    stub.zeroize();
    let torrent = torrent.map_err(|_| mismatch("wrapper", "torrent"))?;
    let kept = kept_files(&identity);
    let kept = kept
        .iter()
        .map(|(path, length)| (path.as_str(), *length))
        .collect::<Vec<_>>();
    blizzard::check_pin(&torrent, &pin, &kept).map_err(|difference| {
        mismatch(
            "torrent",
            match difference {
                blizzard::Mismatch::Infohash => "infohash",
                blizzard::Mismatch::Name => "name",
                blizzard::Mismatch::Pieces => "pieces",
                blizzard::Mismatch::Server => "server",
                blizzard::Mismatch::Files => "files",
            },
        )
    })?;
    let cookie = cookie.ok_or_else(|| mismatch("wrapper", "cookie"))?;
    let now = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    // With the device's clock: a wrong clock shows here.
    eprintln!(
        "pd2-launcher: direct download: {} is the pinned torrent {}; its access expires at {} (Unix time; the device's clock says {now})",
        torrent.name,
        torrent.infohash_hex(),
        cookie.expires()
    );
    if cookie.expires() <= now.saturating_add(COOKIE_MARGIN.as_secs()) {
        eprintln!(
            "pd2-launcher: direct download: {}'s access expires too soon",
            torrent.name
        );
        return Err(VendorFailure::DirectDownload(DirectFailure::Refused));
    }
    let staging = cache
        .ensure_directory(Path::new(&staging_name(&identity)))
        .map_err(|_| VendorFailure::Install)?;
    let directx = staging
        .ensure_directory(Path::new(DIRECTX_DIRECTORY))
        .map_err(|_| VendorFailure::Install)?;
    let files = torrent
        .files
        .iter()
        .map(|file| {
            if file.path.starts_with(blizzard::SKIPPED_PREFIX) {
                return Ok(None);
            }
            // The pin allows only these two folders.
            let (folder, name) = match file.path.split_once('/') {
                None => (&staging, file.path.as_str()),
                Some((DIRECTX_DIRECTORY, name)) => (&directx, name),
                Some(_) => return Err(VendorFailure::Install),
            };
            payload_file(folder, name, file.length)
                .map(Some)
                .map_err(|error| {
                    eprintln!(
                        "pd2-launcher: direct download: {} could not be made: {}",
                        file.path,
                        error.kind()
                    );
                    VendorFailure::Install
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Prepared {
        kind,
        identity,
        pin,
        torrent,
        cookie,
        staging,
        files,
    })
}

/// The files a complete installer keeps, with their pinned sizes.
fn kept_files(identity: &CompletePayloadIdentity) -> Vec<(String, u64)> {
    let mut kept = vec![
        (PAYLOAD_INSTALLER.to_owned(), identity.installer_size),
        (PAYLOAD_TOME.to_owned(), identity.tome_size),
    ];
    kept.extend(
        identity
            .directx_files
            .iter()
            .map(|file| (format!("{DIRECTX_DIRECTORY}/{}", file.name), file.size)),
    );
    kept
}

/// One payload file, opened or made, at full length with its space
/// reserved, so that no piece fails for space later on.
fn payload_file(folder: &Directory, name: &str, length: u64) -> io::Result<PayloadFile> {
    let fresh = match folder.open_file(Path::new(name)) {
        Ok(_) => false,
        Err(error) if error.kind() == io::ErrorKind::NotFound => true,
        Err(error) => return Err(error),
    };
    let file = folder.open_or_create_file(OsStr::new(name))?;
    let metadata = file.metadata()?;
    if metadata.nlink() != 1 {
        return Err(io::Error::other("a payload file has more than one link"));
    }
    if metadata.len() > length {
        file.set_len(length)?;
    }
    match rustix::fs::fallocate(&file, FallocateFlags::empty(), 0, length) {
        Ok(()) => {}
        Err(rustix::io::Errno::OPNOTSUPP | rustix::io::Errno::NOSYS) => file.set_len(length)?,
        Err(error) => return Err(error.into()),
    }
    Ok(PayloadFile { file, fresh })
}

fn synced(files: &[Option<PayloadFile>]) -> bool {
    files
        .iter()
        .flatten()
        .all(|payload| payload.file.sync_all().is_ok())
}

/// Keep a finished payload: its files checked against their identity, then
/// its folder made the product's complete installer.
fn finish(
    cache: &Directory,
    product: Prepared,
    outcome: Outcome,
    control: &OperationControl,
) -> Result<(), VendorFailure> {
    match outcome {
        Outcome::Complete => {}
        Outcome::Refused | Outcome::Expired => {
            return Err(VendorFailure::DirectDownload(DirectFailure::Refused));
        }
        Outcome::Incomplete => {
            return Err(VendorFailure::DirectDownload(DirectFailure::Incomplete));
        }
        Outcome::StoreFailed => return Err(VendorFailure::Install),
    }
    if !synced(&product.files) {
        return Err(VendorFailure::Install);
    }
    drop(product.files);
    if let Err(problem) =
        check_complete_payload_directory_until(&product.staging, product.identity, false, &|| {
            control.is_cancelled()
        })
    {
        if problem.check == "stopped" {
            return Err(VendorFailure::Install);
        }
        crate::activity::record_payload_problem(
            product.identity.source_directory,
            &problem.file,
            problem.check,
            problem.size,
        );
        // Every piece had its SHA-1, so the same pieces would give the same
        // file again: it goes, and the next attempt fetches it anew.
        discard_payload_file(cache, Path::new(&staging_name(&product.identity)), &problem);
        return Err(VendorFailure::DirectDownload(DirectFailure::Mismatch));
    }
    promote(cache, &product.staging, &product.identity)
}

/// Make the checked folder the product's complete installer. A folder
/// already there did not check (the acquisition would have used it), so it
/// is replaced.
fn promote(
    cache: &Directory,
    staging: &Directory,
    identity: &CompletePayloadIdentity,
) -> Result<(), VendorFailure> {
    let name = OsStr::new(identity.cache_directory);
    match cache.open_directory(Path::new(name)) {
        Ok(_) => cache
            .remove_owned_tree(name)
            .map_err(|_| VendorFailure::Install)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(_) => return Err(VendorFailure::Install),
    }
    cache
        .rename_directory_noreplace(OsStr::new(&staging_name(identity)), staging, cache, name)
        .map_err(|_| VendorFailure::Install)?;
    // The direct download made it: a discard removes it.
    cache
        .create_file(OsStr::new(&direct_marker(identity)))
        .and_then(|marker| marker.sync_all())
        .and_then(|()| cache.sync())
        .map_err(|_| VendorFailure::Install)
}

/// Remove the direct download's part of the downloads: both unfinished
/// folders, and each complete installer it made (by its mark; one imported
/// or from Blizzard's downloader stays). The installer goes before its
/// mark, so a stop part way leaves a mark the next discard still follows.
pub(super) fn discard(paths: &ManagedPaths) -> io::Result<()> {
    let cache = match paths
        .directory()
        .open_directory(Path::new("cache/artifacts"))
    {
        Ok(cache) => cache,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let remove_tree = |name: &OsStr| match cache.open_directory(Path::new(name)) {
        Ok(_) => cache.remove_owned_tree(name),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    };
    for kind in [
        ArtifactKind::BaseGameWrapper,
        ArtifactKind::ExpansionWrapper,
    ] {
        let identity = complete_payload_identity(kind)
            .ok_or_else(|| io::Error::other("no payload identity"))?;
        remove_tree(OsStr::new(&staging_name(&identity)))?;
        let marker = direct_marker(&identity);
        match cache.open_file(Path::new(&marker)) {
            Ok(file) => {
                remove_tree(OsStr::new(identity.cache_directory))?;
                cache.remove_open_file_bounded(OsStr::new(&marker), &file)?;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

/// The native install of one product from its verified complete installer:
/// its install script run into the game folder with the checked key and
/// the owner name `Player`. Diablo II is built in `<game>.partial`, which
/// becomes the game folder once complete, so a stop leaves no game behind.
/// Lord of Destruction goes into that folder in place; its archive addition
/// cannot break the archive, and the stage's marker makes a stopped one
/// start over.
/// The Managed game folder's parent below the managed root, made when it is
/// missing, and the game folder's name: the prefix's `Program Files (x86)` on
/// the desktop, the managed root itself on the Android host, whose game
/// folder is outside the prefix.
fn game_parent(paths: &ManagedPaths) -> io::Result<(Directory, OsString)> {
    let not_ours = || {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "the game folder is not the Launcher's",
        )
    };
    let relative = paths
        .game()
        .strip_prefix(paths.root())
        .map_err(|_| not_ours())?;
    let (Some(parent), Some(name)) = (relative.parent(), relative.file_name()) else {
        return Err(not_ours());
    };
    let directory = if parent.as_os_str().is_empty() {
        paths.directory().try_clone()
    } else {
        paths.directory().ensure_directory(parent)
    }
    .map_err(|_| io::Error::other("the game folder's parent could not be made"))?;
    Ok((directory, name.to_owned()))
}

pub(super) fn install_native(
    request: VendorInstallRequest<'_>,
    control: &OperationControl,
) -> Result<(), VendorFailure> {
    let paths = request.paths();
    let (expected, phase) = match request.product() {
        VendorProduct::BaseGame => (blizzard::BASE_GAME_PLAN, WorkPhase::BaseGameInstall),
        VendorProduct::Expansion => (blizzard::EXPANSION_PLAN, WorkPhase::ExpansionInstall),
    };
    let failed = |what: &str| {
        eprintln!("pd2-launcher: native install: {what}");
        VendorFailure::Install
    };
    let payload = match managed_complete_payload_directory_until(request.artifact(), paths, &|| {
        control.is_cancelled()
    }) {
        Ok(payload) => payload,
        Err(failure) => {
            // Stopped during the check: nothing was written.
            control.mark_cancelled_safe();
            return Err(failure.into());
        }
    }
    .ok_or_else(|| failed("no verified complete installer"))?;
    let tome = Directory::open(&payload)
        .and_then(|folder| folder.open_file(Path::new(PAYLOAD_TOME)))
        .map_err(|_| failed("the Tome could not be opened"))?;
    let tome =
        blizzard::Archive::open(tome).map_err(|error| failed(&format!("the Tome: {error:?}")))?;
    let script = tome
        .member(blizzard::SCRIPT_MEMBER)
        .ok()
        .and_then(|script| String::from_utf8(script).ok())
        .ok_or_else(|| failed("no install script"))?;
    let plan = blizzard::read_script(&script)
        .map_err(|error| failed(&format!("the install script: {error:?}")))?;
    blizzard::check_plan(&plan, &expected)
        .map_err(|error| failed(&format!("the install script: {error:?}")))?;
    let cancelled = || control.is_cancelled();
    let mut progress = |done, total| {
        control.report(WorkProgress::for_phase(phase).with_file_bytes(done, Some(total)));
    };
    let outcome = match request.product() {
        VendorProduct::BaseGame => {
            let (parent, name) = game_parent(paths).map_err(|error| failed(&error.to_string()))?;
            let name = name.as_os_str();
            // A game folder with files is never written over: refused
            // before anything is written.
            let empty_game_there = match parent.open_directory(Path::new(name)) {
                Ok(folder) if folder.names().is_ok_and(|names| names.is_empty()) => true,
                Ok(_) => return Err(failed("the game folder has files already")),
                Err(error) if error.kind() == io::ErrorKind::NotFound => false,
                Err(_) => return Err(failed("the game folder could not be opened")),
            };
            let mut partial_name = name.to_owned();
            partial_name.push(".partial");
            match parent.open_directory(Path::new(&partial_name)) {
                Ok(_) => parent
                    .remove_owned_tree(&partial_name)
                    .map_err(|_| failed("an earlier partial game folder stayed"))?,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(_) => return Err(failed("the partial game folder could not be opened")),
            }
            let partial = parent
                .ensure_directory(Path::new(&partial_name))
                .map_err(|_| failed("the partial game folder could not be made"))?;
            let ran = request
                .with_product_key(|key| {
                    blizzard::run_native(&tome, &plan, &partial, key, &cancelled, &mut progress)
                })
                .and_then(|()| {
                    // Its entries on disk before it becomes the game folder.
                    partial.sync()?;
                    if empty_game_there {
                        parent.remove_empty_directory(name)?;
                    }
                    parent.rename_directory_noreplace(&partial_name, &partial, &parent, name)?;
                    parent.sync()?;
                    Ok(())
                });
            if ran.is_err() {
                // The partial folder holds the wrapped key: it goes on
                // every error.
                drop(partial);
                if let Err(error) = parent.remove_owned_tree(&partial_name) {
                    eprintln!(
                        "pd2-launcher: native install: the partial game folder stayed: {}",
                        error.kind()
                    );
                }
            }
            if matches!(ran, Err(blizzard::NativeError::Cancelled)) {
                // Only the partial folder was written, and it is gone.
                control.mark_cancelled_safe();
            }
            ran
        }
        VendorProduct::Expansion => {
            let game = paths
                .open_game()
                .map_err(|_| failed("the game folder could not be opened"))?;
            let ran = request.with_product_key(|key| {
                blizzard::run_native(&tome, &plan, &game, key, &cancelled, &mut progress)
            });
            if matches!(ran, Err(blizzard::NativeError::Cancelled)) {
                // The stage's files are part written: its marker asks for a
                // start over.
                control.mark_cancel_unproven();
            }
            ran
        }
    };
    outcome.map_err(|error| failed(&error.to_string()))
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::FileExt;

    use super::*;

    const FAKE_COOKIE: &str = "fake_cookie=expires=1900000000~access=/applications/x/*~md5=0123456789abcdef0123456789ABCDEF";

    fn wrapper(cache: &Path, stub: &[u8]) -> VerifiedArtifact {
        let path = cache.join("d2-wrapper.verified");
        fs::write(&path, stub).unwrap();
        let sha256 = hex(&Sha256::digest(stub));
        VerifiedArtifact {
            kind: ArtifactKind::BaseGameWrapper,
            path,
            origin: ArtifactOrigin::ApprovedNetwork,
            observed_sha256: sha256.clone(),
            retained_receipt_sha256: Some(sha256),
            requested_url: String::new(),
            final_url: String::new(),
            custom_source: None,
        }
    }

    fn stub(torrent: &[u8], cookie: &str) -> Vec<u8> {
        let mut stub = b"MZ wrapper ".to_vec();
        stub.extend_from_slice(torrent);
        stub.extend_from_slice(b"TAGA\x13\x18\xfe\xff");
        stub.extend_from_slice(cookie.as_bytes());
        stub.extend_from_slice(b"TAGB\x97\xff\xff\xff");
        stub
    }

    /// A small torrent that is not Blizzard's.
    fn synthetic_torrent() -> Vec<u8> {
        let hashes = [0_u8; 20];
        let mut torrent = b"d8:announce3:abc15:direct download".to_vec();
        torrent.extend_from_slice(
            format!(
                "{}:{}",
                blizzard::BASE_GAME.base.len(),
                blizzard::BASE_GAME.base
            )
            .as_bytes(),
        );
        torrent.extend_from_slice(b"4:infod5:filesld6:lengthi10e4:pathl13:Installer.exeeee4:name23:D2-1.14b-Installer-enUS12:piece lengthi16e6:pieces20:");
        torrent.extend_from_slice(&hashes);
        torrent.extend_from_slice(b"ee");
        torrent
    }

    #[test]
    fn a_torrent_that_is_not_the_pinned_one_fails_closed() {
        let root = tempfile::tempdir().unwrap();
        let cache = Directory::open(root.path()).unwrap();
        let artifact = wrapper(root.path(), &stub(&synthetic_torrent(), FAKE_COOKIE));
        assert!(matches!(
            prepare(&cache, &artifact, SystemTime::now()),
            Err(VendorFailure::DirectDownload(DirectFailure::Mismatch))
        ));
        // Nothing was made for it.
        assert!(!root.path().join("d2-payload.partial").exists());
        // No torrent at all: the same.
        let artifact = wrapper(root.path(), &stub(b"no torrent", FAKE_COOKIE));
        assert!(matches!(
            prepare(&cache, &artifact, SystemTime::now()),
            Err(VendorFailure::DirectDownload(DirectFailure::Mismatch))
        ));
    }

    #[test]
    fn the_pinned_files_are_the_complete_installers_files() {
        let base = kept_files(&complete_payload_identity(ArtifactKind::BaseGameWrapper).unwrap());
        assert_eq!(base.len(), 11);
        assert_eq!(
            base.iter().map(|(_, length)| length).sum::<u64>() + 3_386_990,
            blizzard::BASE_GAME.total,
            "the Mac installer's bytes are the rest"
        );
        let expansion =
            kept_files(&complete_payload_identity(ArtifactKind::ExpansionWrapper).unwrap());
        assert_eq!(
            expansion.iter().map(|(_, length)| length).sum::<u64>() + 3_388_672,
            blizzard::EXPANSION.total
        );
        assert!(base.iter().any(|(path, _)| path == "DirectX/dxsetup.exe"));
    }

    #[test]
    fn payload_files_are_made_at_full_length_and_reopened_as_kept() {
        let root = tempfile::tempdir().unwrap();
        let folder = Directory::open(root.path()).unwrap();
        let made = payload_file(&folder, "Installer Tome.mpq", 4096).unwrap();
        assert!(made.fresh);
        assert_eq!(made.file.metadata().unwrap().len(), 4096);
        made.file.write_all_at(b"kept", 100).unwrap();
        drop(made);
        let reopened = payload_file(&folder, "Installer Tome.mpq", 4096).unwrap();
        assert!(!reopened.fresh);
        let mut kept = [0_u8; 4];
        reopened.file.read_exact_at(&mut kept, 100).unwrap();
        assert_eq!(&kept, b"kept");
        // A longer file from elsewhere is cut to the pinned length.
        fs::write(root.path().join("Installer.exe"), vec![1_u8; 9000]).unwrap();
        let cut = payload_file(&folder, "Installer.exe", 4096).unwrap();
        assert_eq!(cut.file.metadata().unwrap().len(), 4096);
    }

    /// One real direct download: both wrappers through the engine's own
    /// acquisition, both installers at the same time into a fresh cache in
    /// `PD2_DIRECT_DOWNLOAD_DIR` (the system's temporary folder when unset),
    /// each kept only once it passes the complete installer's identity
    /// check. About 2.1 GB; the folder is removed afterwards, unless
    /// `PD2_DIRECT_DOWNLOAD_KEEP` names a new folder to use and keep
    /// instead. Run by hand:
    /// `cargo test --release --lib real_direct_download -- --ignored --nocapture`.
    #[test]
    #[ignore = "downloads about 2.1 GB from Blizzard"]
    fn real_direct_download_of_both_installers() {
        let parent =
            env::var_os("PD2_DIRECT_DOWNLOAD_DIR").map_or_else(env::temp_dir, PathBuf::from);
        let temporary = tempfile::tempdir_in(parent).unwrap();
        let kept = env::var_os("PD2_DIRECT_DOWNLOAD_KEEP").map(PathBuf::from);
        let root = kept.as_deref().unwrap_or(temporary.path());
        let paths = ManagedPaths::prepare(root).unwrap();
        fs::create_dir_all(root.join("cache/artifacts")).unwrap();
        let control = OperationControl::default();
        let catalog = ArtifactCatalog::pinned();
        let source = ProductionArtifactSource::new();
        let wrappers = [
            ArtifactKind::BaseGameWrapper,
            ArtifactKind::ExpansionWrapper,
        ]
        .map(|kind| {
            let spec = catalog.spec(kind);
            let acquired = source.acquire(spec, &paths.cache, &control).unwrap();
            verify_artifact(spec, &paths.cache, acquired).unwrap()
        });
        let started = Instant::now();
        let failures = download_payloads(&paths, &[&wrappers[0], &wrappers[1]], &control);
        let elapsed = started.elapsed();
        assert!(failures.is_empty(), "{failures:?}");
        for wrapper in &wrappers {
            let identity = complete_payload_identity(wrapper.kind()).unwrap();
            let payload = Directory::open(&paths.cache.join(identity.cache_directory)).unwrap();
            assert_eq!(
                check_complete_payload_directory(&payload, identity, false),
                Ok(())
            );
            assert!(!paths.cache.join(staging_name(&identity)).exists());
        }
        let bytes = blizzard::BASE_GAME.total + blizzard::EXPANSION.total;
        eprintln!(
            "direct download test: both installers fetched, checked and kept in {:.1} s ({:.1} MB/s over the whole payloads)",
            elapsed.as_secs_f64(),
            bytes as f64 / 1_000_000.0 / elapsed.as_secs_f64()
        );
    }

    /// The native install below the key check, from the payloads that
    /// `real_direct_download_of_both_installers` kept in
    /// `PD2_DIRECT_DOWNLOAD_KEEP`, with clearly fake keys that the full check
    /// refuses: the game goes into that folder's managed game folder. Run by
    /// hand: `PD2_DIRECT_DOWNLOAD_KEEP=<folder> cargo test --release --lib
    /// native_install_of_the_kept -- --ignored --nocapture`.
    #[test]
    #[ignore = "needs the payloads that the real direct download kept"]
    fn native_install_of_the_kept_payloads_with_fake_keys() {
        let root = PathBuf::from(env::var_os("PD2_DIRECT_DOWNLOAD_KEEP").expect("the kept folder"));
        let paths = ManagedPaths::prepare(&root).unwrap();
        let authorization = InstallationAuthorization::new(
            blizzard::TEST_BASE_KEY.to_owned(),
            blizzard::TEST_EXPANSION_KEY.to_owned(),
            true,
        )
        .unwrap();
        let control = OperationControl::default();
        for (product, kind, field) in [
            (
                VendorProduct::BaseGame,
                ArtifactKind::BaseGameWrapper,
                blizzard::KeyField::BaseGame,
            ),
            (
                VendorProduct::Expansion,
                ArtifactKind::ExpansionWrapper,
                blizzard::KeyField::Expansion,
            ),
        ] {
            let identity = complete_payload_identity(kind).unwrap();
            let installer = paths
                .cache
                .join(identity.cache_directory)
                .join(PAYLOAD_INSTALLER);
            let key = authorization.key_for(product);
            assert!(
                blizzard::check_without_test_keys(field, key.expose()).is_err(),
                "a fake key must fail the full check"
            );
            let artifact = VerifiedArtifact {
                kind,
                path: installer,
                origin: ArtifactOrigin::ApprovedNetwork,
                observed_sha256: identity.installer_sha256.to_owned(),
                retained_receipt_sha256: Some(identity.installer_sha256.to_owned()),
                requested_url: String::new(),
                final_url: String::new(),
                custom_source: None,
            };
            let started = Instant::now();
            install_native(
                VendorInstallRequest {
                    product,
                    artifact: &artifact,
                    paths: &paths,
                    product_key: key,
                },
                &control,
            )
            .unwrap();
            eprintln!(
                "native install test: {product:?} installed in {:.1} s",
                started.elapsed().as_secs_f64()
            );
        }
        eprintln!(
            "native install test: the game is in {}",
            paths.game().display()
        );
    }

    #[test]
    fn the_game_folders_parent_is_the_managed_root_on_android_and_the_prefix_on_the_desktop() {
        let root = tempfile::tempdir().unwrap();
        let (parent, name) =
            game_parent(&ManagedPaths::prepare_on(root.path(), true).unwrap()).unwrap();
        assert_eq!(name, OsString::from("Diablo II"));
        let parent = parent.try_clone_file().unwrap().metadata().unwrap();
        let expected = fs::metadata(root.path()).unwrap();
        use std::os::unix::fs::MetadataExt;
        assert_eq!(
            (parent.dev(), parent.ino()),
            (expected.dev(), expected.ino())
        );
        let (_, name) =
            game_parent(&ManagedPaths::prepare_on(root.path(), false).unwrap()).unwrap();
        assert_eq!(name, OsString::from("Diablo II"));
        assert!(
            root.path()
                .join("prefix/drive_c/Program Files (x86)")
                .is_dir()
        );
    }

    #[test]
    fn a_checked_folder_replaces_one_that_did_not_check() {
        let root = tempfile::tempdir().unwrap();
        let cache = Directory::open(root.path()).unwrap();
        let identity = complete_payload_identity(ArtifactKind::ExpansionWrapper).unwrap();
        fs::create_dir_all(root.path().join("lod-payload/DirectX")).unwrap();
        fs::write(root.path().join("lod-payload/Installer.exe"), b"old").unwrap();
        fs::create_dir_all(root.path().join("lod-payload.partial/DirectX")).unwrap();
        fs::write(
            root.path().join("lod-payload.partial/Installer.exe"),
            b"new",
        )
        .unwrap();
        let staging = cache
            .open_directory(Path::new("lod-payload.partial"))
            .unwrap();
        promote(&cache, &staging, &identity).unwrap();
        assert!(!root.path().join("lod-payload.partial").exists());
        assert_eq!(
            fs::read(root.path().join("lod-payload/Installer.exe")).unwrap(),
            b"new"
        );
        // Marked as the direct download's.
        assert!(root.path().join("lod-payload.direct").is_file());
    }
}
