//! Getting a verified asset onto disk, and never an unverified one.
//!
//! This module owns the crate's single invariant:
//!
//! > **Unverified bytes never become a usable file.**
//!
//! The mechanism is download-to-temporary, verify, then rename. The rename is
//! the moment the file exists under its real name, and it happens on exactly one
//! code path -- after [`crate::verify::Verifier::finish`] has returned `Ok`.
//!
//! # Why a temporary file and not "write it and check afterwards"
//!
//! Writing to the final name first means that between the write and the check
//! there is a window where a corrupt or hostile file sits exactly where the
//! loader looks for it. A crash, a power cut, or a second process in that window
//! leaves it there permanently, and the next launch finds a file of the right
//! name and loads it. `architecture.md` section 4 calls a poisoned model
//! *"arbitrary code"*, so that window is not an inconvenience.
//!
//! A failed download must also leave **nothing** behind that a later run could
//! mistake for a partial success, which is why the temporary is removed on every
//! failure path rather than left for a resume that does not exist.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::fetch::{FetchError, Fetcher, Progress};
use crate::manifest::{Asset, AssetManifest};
use crate::verify::{Verifier, VerifyError, digest_of};

/// Why an asset could not be installed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum InstallError {
    /// The bytes did not match the manifest.
    #[error("{file_name} failed verification: {source}")]
    Verification {
        /// Which asset.
        file_name: String,
        /// What was wrong with it.
        source: VerifyError,
    },
    /// The bytes could not be obtained.
    #[error("{file_name} could not be fetched: {source}")]
    Fetch {
        /// Which asset.
        file_name: String,
        /// What the transport said.
        source: FetchError,
    },
    /// The filesystem refused.
    #[error("{file_name}: {operation} failed: {reason}")]
    Filesystem {
        /// Which asset.
        file_name: String,
        /// What was being attempted.
        operation: &'static str,
        /// The operating system's description.
        reason: String,
    },
}

/// Where assets are installed, and the manifest describing them.
#[derive(Debug, Clone)]
pub struct Installer {
    directory: PathBuf,
    manifest: AssetManifest,
}

/// What an asset's state on disk is, before anything is downloaded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssetState {
    /// Present, and its bytes hash to the pinned digest.
    Verified,
    /// Present, but its bytes do **not** match. Treated as absent and replaced.
    Corrupt,
    /// Not on disk.
    Missing,
}

impl Installer {
    /// An installer that puts `manifest`'s assets in `directory`.
    #[must_use]
    pub fn new(directory: PathBuf, manifest: AssetManifest) -> Self {
        Self {
            directory,
            manifest,
        }
    }

    /// Where an asset lives once installed.
    ///
    /// Safe to join because [`Asset::validate`] has already refused any name
    /// with a separator or a traversal component -- and the manifest cannot be
    /// constructed without that check having run.
    #[must_use]
    pub fn path_for(&self, asset: &Asset) -> PathBuf {
        self.directory.join(&asset.file_name)
    }

    /// Whether an asset is already present and still correct.
    ///
    /// Re-hashes the file rather than trusting its presence or its size. A file
    /// of the right length whose contents changed is exactly the case a size
    /// check misses, and it is cheap to be certain: this runs once per launch,
    /// not once per frame.
    ///
    /// An unreadable file reports [`AssetState::Missing`] rather than raising:
    /// from the caller's point of view there is nothing usable there, and the
    /// remedy -- fetch it -- is the same.
    #[must_use]
    pub fn state_of(&self, asset: &Asset) -> AssetState {
        let path = self.path_for(asset);
        match fs::read(&path) {
            Err(_) => AssetState::Missing,
            Ok(bytes) => {
                if bytes.len() as u64 == asset.size_bytes && digest_of(&bytes) == asset.digest {
                    AssetState::Verified
                } else {
                    AssetState::Corrupt
                }
            }
        }
    }

    /// Which assets still need fetching.
    #[must_use]
    pub fn outstanding(&self) -> Vec<&Asset> {
        self.manifest
            .assets
            .iter()
            .filter(|asset| self.state_of(asset) != AssetState::Verified)
            .collect()
    }

    /// Whether every asset is present and verified.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.outstanding().is_empty()
    }

    /// Fetches and installs everything outstanding, reporting progress.
    ///
    /// `on_progress` is called after each chunk and after each completed asset,
    /// so a caller can drive a progress bar without polling. It is not called
    /// for assets already verified on disk, beyond their contribution to the
    /// totals.
    ///
    /// # Errors
    ///
    /// The first [`InstallError`]. **Assets installed before the failure stay
    /// installed** -- they are individually verified, so they are individually
    /// valid, and discarding them would make a flaky connection cost the whole
    /// download every time.
    pub fn install_outstanding<F>(
        &self,
        fetcher: &mut dyn Fetcher,
        mut on_progress: F,
    ) -> Result<(), InstallError>
    where
        F: FnMut(Progress),
    {
        let total_bytes = self.manifest.total_bytes();
        let total_assets = self.manifest.assets.len();
        let mut done_bytes: u64 = 0;
        let mut done_assets = 0_usize;

        for asset in &self.manifest.assets {
            if self.state_of(asset) == AssetState::Verified {
                done_bytes = done_bytes.saturating_add(asset.size_bytes);
                done_assets += 1;
                on_progress(Progress {
                    done_bytes,
                    total_bytes,
                    done_assets,
                    total_assets,
                });
                continue;
            }

            let before = done_bytes;
            self.install_one(fetcher, asset, |received| {
                on_progress(Progress {
                    done_bytes: before.saturating_add(received),
                    total_bytes,
                    done_assets,
                    total_assets,
                });
            })?;
            done_bytes = before.saturating_add(asset.size_bytes);
            done_assets += 1;
            on_progress(Progress {
                done_bytes,
                total_bytes,
                done_assets,
                total_assets,
            });
        }
        Ok(())
    }

    /// Fetches one asset into a temporary file, verifies it, and only then
    /// renames it into place.
    ///
    /// # Errors
    ///
    /// [`InstallError`]. On **every** failure path the temporary file is
    /// removed, so a failed install leaves nothing a later run could mistake for
    /// a partial success.
    pub fn install_one<F>(
        &self,
        fetcher: &mut dyn Fetcher,
        asset: &Asset,
        mut on_bytes: F,
    ) -> Result<(), InstallError>
    where
        F: FnMut(u64),
    {
        let filesystem =
            |operation: &'static str, error: &std::io::Error| InstallError::Filesystem {
                file_name: asset.file_name.clone(),
                operation,
                reason: error.to_string(),
            };

        fs::create_dir_all(&self.directory)
            .map_err(|error| filesystem("creating the install directory", &error))?;

        let temporary = self.temporary_path_for(asset);
        // A stale temporary from a previous crash must not be appended to.
        let _ = fs::remove_file(&temporary);

        let outcome = self.stream_to_temporary(fetcher, asset, &temporary, &mut on_bytes);
        if let Err(error) = outcome {
            let _ = fs::remove_file(&temporary);
            return Err(error);
        }

        let destination = self.path_for(asset);
        // `rename` replaces an existing file on both platforms UP-TAKE targets,
        // which is what makes replacing a Corrupt asset work without a window
        // where neither copy exists.
        if let Err(error) = fs::rename(&temporary, &destination) {
            let _ = fs::remove_file(&temporary);
            return Err(filesystem("renaming into place", &error));
        }
        Ok(())
    }

    /// The temporary name an in-flight download uses.
    ///
    /// Beside the destination rather than in the system temp directory, so the
    /// rename is within one filesystem and therefore atomic. A cross-device
    /// rename silently degrades to copy-then-delete, which reintroduces exactly
    /// the window this design exists to remove.
    fn temporary_path_for(&self, asset: &Asset) -> PathBuf {
        self.directory.join(format!("{}.partial", asset.file_name))
    }

    /// Streams one asset into `temporary`, verifying as it goes.
    fn stream_to_temporary<F>(
        &self,
        fetcher: &mut dyn Fetcher,
        asset: &Asset,
        temporary: &Path,
        on_bytes: &mut F,
    ) -> Result<(), InstallError>
    where
        F: FnMut(u64),
    {
        let filesystem =
            |operation: &'static str, error: &std::io::Error| InstallError::Filesystem {
                file_name: asset.file_name.clone(),
                operation,
                reason: error.to_string(),
            };

        let mut file = fs::File::create(temporary)
            .map_err(|error| filesystem("creating the download", &error))?;
        let mut verifier = Verifier::new(asset.digest, asset.size_bytes);

        // The sink is where verification and writing happen together. A
        // transport has nowhere else to put the bytes, so neither step can be
        // skipped by an implementation of `Fetcher`.
        let mut sink_error: Option<InstallError> = None;
        let fetch_result = fetcher.fetch(asset, &mut |chunk: &[u8]| {
            if let Err(error) = verifier.update(chunk) {
                let wrapped = InstallError::Verification {
                    file_name: asset.file_name.clone(),
                    source: error.clone(),
                };
                let message = wrapped.to_string();
                sink_error = Some(wrapped);
                return Err(message);
            }
            if let Err(error) = file.write_all(chunk) {
                let wrapped = filesystem("writing the download", &error);
                let message = wrapped.to_string();
                sink_error = Some(wrapped);
                return Err(message);
            }
            on_bytes(verifier.received());
            Ok(())
        });

        // A sink error is the real cause; the transport only relayed it.
        if let Some(error) = sink_error {
            return Err(error);
        }
        fetch_result.map_err(|error| InstallError::Fetch {
            file_name: asset.file_name.clone(),
            source: error,
        })?;

        file.flush()
            .map_err(|error| filesystem("flushing the download", &error))?;
        drop(file);

        verifier
            .finish()
            .map_err(|source| InstallError::Verification {
                file_name: asset.file_name.clone(),
                source,
            })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::manifest::AssetKind;
    use std::cell::RefCell;
    use std::rc::Rc;

    /// A fetcher that serves fixed bytes, in fixed-size chunks.
    struct FakeFetcher {
        payload: Vec<u8>,
        chunk_size: usize,
        calls: Rc<RefCell<usize>>,
    }

    impl FakeFetcher {
        fn new(payload: Vec<u8>) -> Self {
            Self {
                payload,
                chunk_size: 7,
                calls: Rc::new(RefCell::new(0)),
            }
        }
    }

    impl Fetcher for FakeFetcher {
        fn fetch(
            &mut self,
            _asset: &Asset,
            sink: &mut dyn FnMut(&[u8]) -> Result<(), String>,
        ) -> Result<(), FetchError> {
            *self.calls.borrow_mut() += 1;
            for chunk in self.payload.chunks(self.chunk_size.max(1)) {
                // A real transport stops when the sink refuses; so does this.
                sink(chunk).map_err(|reason| FetchError::Transport {
                    url: "fake".to_owned(),
                    reason,
                })?;
            }
            Ok(())
        }
    }

    /// A fetcher that always fails at the transport level.
    struct BrokenFetcher;

    impl Fetcher for BrokenFetcher {
        fn fetch(
            &mut self,
            _asset: &Asset,
            _sink: &mut dyn FnMut(&[u8]) -> Result<(), String>,
        ) -> Result<(), FetchError> {
            Err(FetchError::Transport {
                url: "fake".to_owned(),
                reason: "connection reset".to_owned(),
            })
        }
    }

    fn temp_dir(label: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "uptake-assets-test-{label}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default()
        ));
        path
    }

    fn asset_for(payload: &[u8], name: &str) -> Asset {
        Asset {
            file_name: name.to_owned(),
            url: "https://example.test/a".to_owned(),
            digest: digest_of(payload),
            size_bytes: payload.len() as u64,
            kind: AssetKind::Model,
        }
    }

    fn installer_for(directory: &Path, assets: Vec<Asset>) -> Installer {
        Installer::new(directory.to_path_buf(), AssetManifest::new(assets).unwrap())
    }

    #[test]
    fn a_matching_download_is_installed_under_its_real_name() {
        let directory = temp_dir("happy");
        let payload = b"the quick brown fox".to_vec();
        let asset = asset_for(&payload, "model.onnx");
        let installer = installer_for(&directory, vec![asset.clone()]);

        installer
            .install_one(&mut FakeFetcher::new(payload.clone()), &asset, |_| {})
            .unwrap();

        assert_eq!(fs::read(installer.path_for(&asset)).unwrap(), payload);
        assert_eq!(installer.state_of(&asset), AssetState::Verified);
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn a_corrupt_download_never_appears_under_its_real_name() {
        // THE INVARIANT. The bytes hash to something else, so the destination
        // must not exist at all -- not exist-and-be-wrong, and not exist as a
        // leftover the next launch would load.
        let directory = temp_dir("corrupt");
        let asset = asset_for(b"what we asked for", "model.onnx");
        let installer = installer_for(&directory, vec![asset.clone()]);

        let error = installer
            .install_one(
                &mut FakeFetcher::new(b"something else entirely!!".to_vec()),
                &asset,
                |_| {},
            )
            .unwrap_err();

        assert!(
            matches!(error, InstallError::Verification { .. }),
            "got {error}"
        );
        assert!(
            !installer.path_for(&asset).exists(),
            "an unverified file reached its real name"
        );
        assert_eq!(installer.state_of(&asset), AssetState::Missing);
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn bytes_of_the_right_length_but_the_wrong_content_never_reach_the_real_name() {
        // ⚠️ THE TEST THAT ACTUALLY PINS THE INVARIANT, and the one above does
        // NOT. `a_corrupt_download_never_appears_under_its_real_name` serves a
        // payload of a DIFFERENT LENGTH, so the streaming length check in
        // `Verifier::update` rejects it and the digest comparison never runs.
        //
        // Found by mutation, not by reading: swallowing `Verifier::finish`'s
        // result entirely left all 41 tests green. This is the case where only
        // the digest can tell -- same length, different bytes -- which is also
        // the shape a deliberately substituted model file would have, since an
        // attacker controls the length trivially.
        let directory = temp_dir("samelen");
        let wanted = b"the genuine model".to_vec();
        let forged = b"a forged model!!!".to_vec();
        assert_eq!(
            wanted.len(),
            forged.len(),
            "the fixture must match in length"
        );
        assert_ne!(wanted, forged);

        let asset = asset_for(&wanted, "model.onnx");
        let installer = installer_for(&directory, vec![asset.clone()]);

        let error = installer
            .install_one(&mut FakeFetcher::new(forged), &asset, |_| {})
            .unwrap_err();

        match error {
            InstallError::Verification { source, .. } => {
                assert!(
                    matches!(source, VerifyError::DigestMismatch { .. }),
                    "expected the DIGEST to catch this, got {source}"
                );
            }
            other => panic!("expected a verification error, got {other}"),
        }
        assert!(
            !installer.path_for(&asset).exists(),
            "a file that hashed wrong reached its real name"
        );
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn a_same_length_forgery_does_not_survive_as_an_installed_asset() {
        // The second half of the invariant: not merely "the install errored",
        // but "nothing usable is left behind and the next run still fetches".
        let directory = temp_dir("samelen2");
        let wanted = b"the genuine model".to_vec();
        let forged = b"a forged model!!!".to_vec();
        let asset = asset_for(&wanted, "model.onnx");
        let installer = installer_for(&directory, vec![asset.clone()]);

        let _ = installer.install_one(&mut FakeFetcher::new(forged), &asset, |_| {});

        assert_eq!(installer.state_of(&asset), AssetState::Missing);
        assert_eq!(installer.outstanding().len(), 1);
        assert!(!installer.is_complete());
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn a_failed_download_leaves_no_partial_file_behind() {
        let directory = temp_dir("partial");
        let asset = asset_for(b"the whole thing", "model.onnx");
        let installer = installer_for(&directory, vec![asset.clone()]);

        let _ = installer.install_one(&mut BrokenFetcher, &asset, |_| {});

        let leftovers: Vec<String> = fs::read_dir(&directory)
            .map(|entries| {
                entries
                    .filter_map(Result::ok)
                    .map(|entry| entry.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        assert!(
            leftovers.is_empty(),
            "a failed install left files behind: {leftovers:?}"
        );
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn a_transport_failure_is_reported_as_a_fetch_error_not_a_checksum_one() {
        // "the connection dropped" must not read to a user as "the file was
        // tampered with" -- they lead to completely different actions.
        let directory = temp_dir("transport");
        let asset = asset_for(b"payload", "model.onnx");
        let installer = installer_for(&directory, vec![asset.clone()]);

        let error = installer
            .install_one(&mut BrokenFetcher, &asset, |_| {})
            .unwrap_err();
        assert!(matches!(error, InstallError::Fetch { .. }), "got {error}");
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn an_over_long_response_is_cut_off_rather_than_buffered_to_the_end() {
        // The sink refuses at the byte that crosses the declared length, and the
        // transport must stop there. This is what stops a hostile or broken
        // server streaming without limit.
        let directory = temp_dir("toolong");
        let asset = asset_for(b"short", "model.onnx");
        let installer = installer_for(&directory, vec![asset.clone()]);

        let mut oversized = vec![b'x'; 10_000];
        oversized[..5].copy_from_slice(b"short");
        let error = installer
            .install_one(&mut FakeFetcher::new(oversized), &asset, |_| {})
            .unwrap_err();

        match error {
            InstallError::Verification { source, .. } => {
                assert!(
                    matches!(source, VerifyError::TooLong { .. }),
                    "got {source}"
                );
            }
            other => panic!("expected a verification error, got {other}"),
        }
        assert!(!installer.path_for(&asset).exists());
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn an_already_verified_asset_is_not_fetched_again() {
        let directory = temp_dir("cached");
        let payload = b"already here".to_vec();
        let asset = asset_for(&payload, "model.onnx");
        let installer = installer_for(&directory, vec![asset.clone()]);

        let mut fetcher = FakeFetcher::new(payload.clone());
        let calls = Rc::clone(&fetcher.calls);
        installer.install_outstanding(&mut fetcher, |_| {}).unwrap();
        assert_eq!(*calls.borrow(), 1, "first run should fetch");

        installer.install_outstanding(&mut fetcher, |_| {}).unwrap();
        assert_eq!(*calls.borrow(), 1, "second run refetched an intact asset");
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn a_file_edited_after_installation_is_detected_and_replaced() {
        // The case a size check misses: same length, different bytes.
        let directory = temp_dir("tampered");
        let payload = b"original text".to_vec();
        let asset = asset_for(&payload, "model.onnx");
        let installer = installer_for(&directory, vec![asset.clone()]);

        installer
            .install_one(&mut FakeFetcher::new(payload.clone()), &asset, |_| {})
            .unwrap();
        fs::write(installer.path_for(&asset), b"tampered text").unwrap();
        assert_eq!(installer.state_of(&asset), AssetState::Corrupt);

        installer
            .install_outstanding(&mut FakeFetcher::new(payload.clone()), |_| {})
            .unwrap();
        assert_eq!(installer.state_of(&asset), AssetState::Verified);
        assert_eq!(fs::read(installer.path_for(&asset)).unwrap(), payload);
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn a_stale_partial_from_a_previous_crash_is_not_appended_to() {
        let directory = temp_dir("stale");
        let payload = b"clean payload".to_vec();
        let asset = asset_for(&payload, "model.onnx");
        let installer = installer_for(&directory, vec![asset.clone()]);

        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("model.onnx.partial"), b"junk from last time").unwrap();

        installer
            .install_one(&mut FakeFetcher::new(payload.clone()), &asset, |_| {})
            .unwrap();
        assert_eq!(fs::read(installer.path_for(&asset)).unwrap(), payload);
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn progress_reaches_the_totals_and_never_exceeds_them() {
        let directory = temp_dir("progress");
        let first = asset_for(&[b'a'; 100], "a.onnx");
        let second = asset_for(&[b'b'; 50], "b.onnx");
        let installer = installer_for(&directory, vec![first.clone(), second.clone()]);

        // One fetcher cannot serve two different payloads, so install each with
        // its own and watch the aggregate through install_outstanding after.
        installer
            .install_one(&mut FakeFetcher::new(vec![b'a'; 100]), &first, |_| {})
            .unwrap();
        installer
            .install_one(&mut FakeFetcher::new(vec![b'b'; 50]), &second, |_| {})
            .unwrap();

        let seen = RefCell::new(Vec::new());
        installer
            .install_outstanding(&mut FakeFetcher::new(Vec::new()), |progress| {
                seen.borrow_mut().push(progress);
            })
            .unwrap();

        let seen = seen.into_inner();
        assert!(!seen.is_empty(), "no progress was reported");
        for progress in &seen {
            assert!(
                progress.done_bytes <= progress.total_bytes,
                "progress exceeded its total: {progress:?}"
            );
            assert!(progress.fraction() <= 1.0);
        }
        let last = seen.last().unwrap();
        assert_eq!(last.done_bytes, 150);
        assert!(last.is_complete());
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn outstanding_names_only_what_is_missing_or_corrupt() {
        let directory = temp_dir("outstanding");
        let payload = b"present".to_vec();
        let present = asset_for(&payload, "here.onnx");
        let absent = asset_for(b"never fetched", "gone.onnx");
        let installer = installer_for(&directory, vec![present.clone(), absent.clone()]);

        assert_eq!(installer.outstanding().len(), 2);
        assert!(!installer.is_complete());

        installer
            .install_one(&mut FakeFetcher::new(payload), &present, |_| {})
            .unwrap();
        let outstanding = installer.outstanding();
        assert_eq!(outstanding.len(), 1);
        assert_eq!(outstanding[0].file_name, "gone.onnx");
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn install_creates_the_directory_if_it_does_not_exist() {
        let directory = temp_dir("nodir").join("nested").join("deeper");
        let payload = b"payload".to_vec();
        let asset = asset_for(&payload, "model.onnx");
        let installer = installer_for(&directory, vec![asset.clone()]);

        assert!(!directory.exists());
        installer
            .install_one(&mut FakeFetcher::new(payload), &asset, |_| {})
            .unwrap();
        assert!(installer.path_for(&asset).is_file());
        let _ = fs::remove_dir_all(&directory);
    }
}
