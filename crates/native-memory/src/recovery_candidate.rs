//! Unwired read-only installed-state reconciliation candidate.
//!
//! A matching file does not show that a running guest loaded or enforces it;
//! an absent receipt does not prove no effect. This cannot clear a dirty journal,
//! retry a mutation, repair a SQLite hot journal, or prove an off-host witness.
use crate::{
    Error,
    grant_ledger_candidate::{Ledger, LedgerError, RecoveryObservation},
    grant_snapshot_candidate::{ExpectedBinding, LoadedSnapshot},
};

#[derive(Debug, PartialEq, Eq)]
pub enum InspectionError {
    Manager(Error),
    Ledger(LedgerError),
    Installed(Error),
}

#[derive(Debug, PartialEq, Eq)]
pub struct InstalledRecovery {
    pub records: RecoveryObservation,
    pub installed_matches_operation: bool,
    pub installed_generation: u64,
}

fn inspect_with(
    mut manager: impl FnMut() -> crate::Result<()>,
    records: impl FnOnce() -> std::result::Result<Option<RecoveryObservation>, LedgerError>,
    installed: impl FnOnce() -> crate::Result<LoadedSnapshot>,
    expected_digest: &str,
) -> std::result::Result<Option<InstalledRecovery>, InspectionError> {
    manager().map_err(InspectionError::Manager)?;
    let records = records().map_err(InspectionError::Ledger)?;
    let result = match records {
        None => None,
        Some(records) => {
            let snapshot = installed().map_err(InspectionError::Installed)?;
            let digest = snapshot
                .digest()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>();
            Some(InstalledRecovery {
                records,
                installed_matches_operation: digest == expected_digest,
                installed_generation: snapshot.snapshot().generation,
            })
        }
    };
    manager().map_err(InspectionError::Manager)?;
    Ok(result)
}

/// Inspect a root-owned installed snapshot and the root-private ledger. The
/// caller must hold the actual hotel fence and supply trusted operation/binding
/// metadata. Manager observations do not lock lifecycle after this returns.
/// Running this synchronously can block and must not block an async executor.
#[cfg(target_os = "linux")]
pub fn inspect_root_snapshot(
    ledger: &mut Ledger,
    manager: &crate::protected_peer_candidate::ManagerPinnedProcess,
    operation_id: &str,
    expected_digest: &str,
    path: &std::path::Path,
    group: u32,
    binding: &ExpectedBinding<'_>,
) -> std::result::Result<Option<InstalledRecovery>, InspectionError> {
    inspect_with(
        || manager.verify_manager(),
        || ledger.inspect_recovery(operation_id, expected_digest),
        || crate::grant_snapshot_candidate::load_root_owned(path, group, binding),
        expected_digest,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grant_ledger_candidate::InstallationState;
    use sha2::{Digest, Sha256};
    #[test]
    fn changed_manager_and_different_installed_state_do_not_become_success() {
        let binding = crate::grant_snapshot_candidate::tests::expected();
        let sample = crate::grant_snapshot_candidate::tests::sample();
        let bytes = serde_json::to_vec(&sample).unwrap();
        let digest = format!("{:x}", Sha256::digest(&bytes));
        let records = || {
            Ok(Some(RecoveryObservation {
                current_matches: true,
                installation: InstallationState::Pending,
            }))
        };
        let load = || crate::grant_snapshot_candidate::validate_snapshot_bytes(&bytes, &binding);
        let observation = inspect_with(|| Ok(()), records, load, &digest)
            .unwrap()
            .unwrap();
        assert!(observation.installed_matches_operation);
        assert_eq!(observation.records.installation, InstallationState::Pending);
        let observation = inspect_with(
            || Ok(()),
            || {
                Ok(Some(RecoveryObservation {
                    current_matches: false,
                    installation: InstallationState::Complete,
                }))
            },
            || crate::grant_snapshot_candidate::validate_snapshot_bytes(&bytes, &binding),
            &"a".repeat(64),
        )
        .unwrap()
        .unwrap();
        assert!(!observation.installed_matches_operation);
        assert!(!observation.records.current_matches);
        let calls = std::cell::Cell::new(0);
        let manager = || {
            let n = calls.get();
            calls.set(n + 1);
            if n == 0 { Ok(()) } else { Err(Error::Denied) }
        };
        assert_eq!(
            inspect_with(
                manager,
                || Ok(Some(RecoveryObservation {
                    current_matches: true,
                    installation: InstallationState::Pending
                })),
                || crate::grant_snapshot_candidate::validate_snapshot_bytes(&bytes, &binding),
                &digest
            ),
            Err(InspectionError::Manager(Error::Denied))
        );
    }

    #[test]
    fn reads_synthetic_installed_file_and_rejects_custody_drift() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let dir = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o750)).unwrap();
        let metadata = std::fs::metadata(dir.path()).unwrap();
        let path = dir.path().join("snapshot.json");
        let binding = crate::grant_snapshot_candidate::tests::expected();
        let bytes = serde_json::to_vec(&crate::grant_snapshot_candidate::tests::sample()).unwrap();
        std::fs::write(&path, &bytes).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        let digest = format!("{:x}", Sha256::digest(&bytes));
        let records = || {
            Ok(Some(RecoveryObservation {
                current_matches: true,
                installation: InstallationState::Pending,
            }))
        };
        let inspect = || {
            crate::grant_snapshot_candidate::load_test_owned(
                &path,
                metadata.uid(),
                metadata.gid(),
                &binding,
            )
        };
        assert!(
            inspect_with(|| Ok(()), records, inspect, &digest)
                .unwrap()
                .unwrap()
                .installed_matches_operation
        );
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666)).unwrap();
        let inspect = || {
            crate::grant_snapshot_candidate::load_test_owned(
                &path,
                metadata.uid(),
                metadata.gid(),
                &binding,
            )
        };
        assert_eq!(
            inspect_with(|| Ok(()), records, inspect, &digest),
            Err(InspectionError::Installed(Error::Denied))
        );
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        std::fs::write(&path, b"not a snapshot").unwrap();
        let inspect = || {
            crate::grant_snapshot_candidate::load_test_owned(
                &path,
                metadata.uid(),
                metadata.gid(),
                &binding,
            )
        };
        assert_eq!(
            inspect_with(|| Ok(()), records, inspect, &digest),
            Err(InspectionError::Installed(Error::Invalid))
        );
        std::fs::remove_file(&path).unwrap();
        let inspect = || {
            crate::grant_snapshot_candidate::load_test_owned(
                &path,
                metadata.uid(),
                metadata.gid(),
                &binding,
            )
        };
        assert!(matches!(
            inspect_with(|| Ok(()), records, inspect, &digest),
            Err(InspectionError::Installed(_))
        ));
        let called = std::cell::Cell::new(false);
        assert_eq!(
            inspect_with(
                || Ok(()),
                || Ok(None),
                || {
                    called.set(true);
                    Err(Error::Denied)
                },
                &digest
            ),
            Ok(None)
        );
        assert!(!called.get()); // Missing receipt is not a statement about installed state.
        assert_eq!(
            inspect_with(
                || Ok(()),
                || Err(LedgerError::Unavailable),
                || Err(Error::Denied),
                &digest
            ),
            Err(InspectionError::Ledger(LedgerError::Unavailable))
        );
    }
}
