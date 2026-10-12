//! Pure build metadata validation, shared by build.rs and source tests.
#[derive(Debug, PartialEq, Eq)]
pub struct BuildVersion {
    pub version: String,
    pub sha: String,
}

fn numeric(part: &str) -> bool {
    !part.is_empty()
        && part.bytes().all(|byte| byte.is_ascii_digit())
        && (part.len() == 1 || !part.starts_with('0'))
}

pub fn release_version(tag: &str) -> Result<&str, &'static str> {
    let version = tag
        .strip_prefix('v')
        .ok_or("release tag must start with v")?;
    let (core, suffix) = version.split_once('-').unwrap_or((version, ""));
    let components: Vec<_> = core.split('.').collect();
    if components.len() != 3 || !components.iter().all(|part| numeric(part)) {
        return Err("release tag must have three canonical numeric components");
    }
    if version.contains('-') {
        let (channel, number) = suffix.split_once('.').ok_or("invalid release channel")?;
        if !matches!(channel, "alpha" | "beta" | "rc") || !numeric(number) {
            return Err("release channel must be alpha.N, beta.N or rc.N");
        }
    }
    Ok(version)
}

pub fn resolve(
    package_version: &str,
    tag: Option<&str>,
    sha: Option<&str>,
) -> Result<BuildVersion, &'static str> {
    if sha.is_some_and(|value| {
        value.len() != 40
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    }) {
        return Err("PHILOTIC_BUILD_SHA must be a full lowercase Git commit SHA");
    }
    let version = match tag {
        Some(tag) => {
            if sha.is_none() {
                return Err("release builds require PHILOTIC_BUILD_SHA");
            }
            release_version(tag)?.to_owned()
        }
        None => format!("{package_version}-dev"),
    };
    Ok(BuildVersion {
        version,
        sha: sha.unwrap_or("unknown").to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    const SHA: &str = "689bfc7ddce26341cac994bb77d0027bea2ac98a";

    #[test]
    fn rc_build_uses_exact_tag_version_and_commit() {
        assert_eq!(
            resolve("0.1.0", Some("v0.2.0-rc.1"), Some(SHA)).unwrap(),
            BuildVersion {
                version: "0.2.0-rc.1".into(),
                sha: SHA.into()
            }
        );
    }

    #[test]
    fn stable_alpha_and_beta_follow_the_same_contract() {
        for tag in ["v0.2.0", "v0.2.0-alpha.1", "v0.2.0-beta.2"] {
            assert_eq!(
                resolve("0.1.0", Some(tag), Some(SHA)).unwrap().version,
                &tag[1..]
            );
        }
    }

    #[test]
    fn untagged_build_does_not_claim_an_uncreated_release() {
        let local = resolve("0.1.0", None, None).unwrap();
        assert_eq!(local.version, "0.1.0-dev");
        assert_eq!(local.sha, "unknown");
        assert_eq!(resolve("0.1.0", None, Some(SHA)).unwrap().sha, SHA);
    }

    #[test]
    fn invalid_or_incomplete_release_metadata_fails() {
        assert!(resolve("0.1.0", Some("v0.2.0-rc.1"), None).is_err());
        for sha in [
            "",
            "689bfc7",
            "not-a-commit",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        ] {
            assert!(resolve("0.1.0", None, Some(sha)).is_err());
        }
        for tag in [
            "",
            "0.2.0",
            "v0.2",
            "v0.2.0-rc1",
            "v0.2.0-rc.01",
            "v00.2.0",
            "v0.2.0-rc.1-extra",
        ] {
            assert!(release_version(tag).is_err(), "{tag}");
        }
    }
}
