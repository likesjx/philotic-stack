use crate::policy::SandboxPolicy;
use anyhow::{Context, Result};
use landlock::{
    Access, AccessFs, PathBeneath, PathFd, Ruleset, RulesetAttr, RulesetCreatedAttr, RulesetStatus,
    ABI,
};
use tracing::{info, warn};

/// Target Landlock ABI version.
const TARGET_ABI: ABI = ABI::V3;

/// Apply Landlock filesystem restrictions based on the sandbox policy.
/// Returns Ok(true) if Landlock was applied, Ok(false) if unavailable.
pub fn apply_landlock(policy: &SandboxPolicy, strict: bool) -> Result<bool> {
    let status = match Ruleset::default()
        .handle_access(AccessFs::from_all(TARGET_ABI))
        .context("failed to create landlock ruleset")
    {
        Ok(ruleset) => {
            // add_rule consumes self and does not return it on error, so we must
            // propagate failures with ? rather than warn-and-continue.  Missing paths
            // (PathFd failure) are still warned-and-skipped since they're common.
            let mut created = ruleset
                .create()
                .context("failed to create landlock ruleset")?;

            for pattern in &policy.filesystem.read_paths {
                if let Some(path) = resolve_pattern_to_path(pattern) {
                    match PathFd::new(&path) {
                        Ok(fd) => {
                            let access = AccessFs::ReadFile | AccessFs::ReadDir;
                            created = created
                                .add_rule(PathBeneath::new(fd, access))
                                .context("failed to add read rule")?;
                        }
                        Err(e) => warn!("landlock: cannot open path {}: {}", path, e),
                    }
                }
            }

            for pattern in &policy.filesystem.write_paths {
                if let Some(path) = resolve_pattern_to_path(pattern) {
                    match PathFd::new(&path) {
                        Ok(fd) => {
                            let access = AccessFs::ReadFile
                                | AccessFs::ReadDir
                                | AccessFs::WriteFile
                                | AccessFs::MakeDir
                                | AccessFs::MakeReg;
                            created = created
                                .add_rule(PathBeneath::new(fd, access))
                                .context("failed to add write rule")?;
                        }
                        Err(e) => warn!("landlock: cannot open path {}: {}", path, e),
                    }
                }
            }

            for pattern in &policy.filesystem.execute_paths {
                if let Some(path) = resolve_pattern_to_path(pattern) {
                    match PathFd::new(&path) {
                        Ok(fd) => {
                            let access = AccessFs::ReadFile | AccessFs::ReadDir | AccessFs::Execute;
                            created = created
                                .add_rule(PathBeneath::new(fd, access))
                                .context("failed to add execute rule")?;
                        }
                        Err(e) => warn!("landlock: cannot open path {}: {}", path, e),
                    }
                }
            }

            created
                .restrict_self()
                .context("failed to apply landlock restrictions")?
        }
        Err(e) => {
            if strict {
                return Err(e);
            }
            warn!("landlock: not available on this kernel: {}", e);
            return Ok(false);
        }
    };

    match status.ruleset {
        RulesetStatus::FullyEnforced => {
            info!("landlock: fully enforced");
            Ok(true)
        }
        RulesetStatus::PartiallyEnforced => {
            if strict {
                anyhow::bail!(
                    "landlock: only partially enforced, but --strict requires full enforcement"
                );
            }
            warn!("landlock: only partially enforced (kernel may be too old for some features)");
            Ok(true)
        }
        RulesetStatus::NotEnforced => {
            if strict {
                anyhow::bail!("landlock: not enforced, but --strict requires enforcement");
            }
            warn!("landlock: not enforced");
            Ok(false)
        }
    }
}

/// Resolve a glob pattern to its base directory path (strip trailing `/**`).
fn resolve_pattern_to_path(pattern: &str) -> Option<String> {
    let path = pattern
        .trim_end_matches("/**")
        .trim_end_matches("/*")
        .trim_end_matches('*');

    if path.is_empty() {
        return Some("/".to_string());
    }

    Some(path.to_string())
}
