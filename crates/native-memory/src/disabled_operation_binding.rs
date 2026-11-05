//! Disabled, non-secret publication binding. This loader is not wired into
//! the hotel and cannot authorize a process, publish grants, or reopen access.
use crate::{Error, Result, protected_config};
use serde::Deserialize;
use std::path::Path;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DisabledOperationBinding {
    version: u8,
    enabled: bool,
    operation_id: String,
    hotel: String,
    endpoint: String,
    owner: String,
    subject: String,
    client: String,
    issuer: String,
    resource: String,
    namespace: String,
    runner: String,
    expected_publisher_service: String,
    expected_publisher_uid: u32,
    expected_publisher_executable_sha256: String,
    old_endpoint_sha256: String,
    new_endpoint_sha256: String,
    old_ingress_sha256: String,
    new_ingress_sha256: String,
    old_receiver_sha256: String,
    new_receiver_sha256: String,
    old_revision: u64,
    new_revision: u64,
    subject_version: u64,
    client_version: u64,
    resource_version: u64,
    deadline: u64,
    expires_at: u64,
    actions: [String; 2],
}

/// Non-secret expected hotel coordinates. These are comparisons, not proof
/// of custody or process identity; their caller must establish their source.
#[derive(Clone, Copy)]
pub struct ExpectedHotelBinding<'a> {
    pub hotel: &'a str,
    pub endpoint: &'a str,
    pub owner: &'a str,
    pub subject: &'a str,
    pub client: &'a str,
    pub issuer: &'a str,
    pub resource: &'a str,
    pub namespace: &'a str,
    pub runner: &'a str,
}

impl DisabledOperationBinding {
    fn parse(bytes: &[u8]) -> Result<Self> {
        let value: Self = serde_json::from_slice(bytes).map_err(|_| Error::Invalid)?;
        let label = |s: &str| {
            !s.is_empty()
                && s.len() <= 128
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
        };
        let hash = |s: &str| {
            s.len() == 64
                && s.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        };
        let uuid =
            |s: &str| uuid::Uuid::parse_str(s).is_ok_and(|u| !u.is_nil() && u.to_string() == s);
        let https = |s: &str| {
            url::Url::parse(s).is_ok_and(|u| {
                u.scheme() == "https"
                    && u.host_str().is_some()
                    && u.username().is_empty()
                    && u.password().is_none()
                    && u.query().is_none()
                    && u.fragment().is_none()
            })
        };
        if value.version != 1
            || value.enabled
            || !uuid(&value.operation_id)
            || !uuid(&value.subject)
            || ![
                &value.hotel,
                &value.endpoint,
                &value.owner,
                &value.client,
                &value.namespace,
                &value.runner,
                &value.expected_publisher_service,
            ]
            .iter()
            .all(|s| label(s))
            || !https(&value.issuer)
            || !https(&value.resource)
            || ![
                &value.expected_publisher_executable_sha256,
                &value.old_endpoint_sha256,
                &value.new_endpoint_sha256,
                &value.old_ingress_sha256,
                &value.new_ingress_sha256,
                &value.old_receiver_sha256,
                &value.new_receiver_sha256,
            ]
            .iter()
            .all(|s| hash(s))
            || value.old_endpoint_sha256 == value.new_endpoint_sha256
            || value.old_ingress_sha256 == value.new_ingress_sha256
            || value.old_receiver_sha256 == value.new_receiver_sha256
            || value.old_revision.checked_add(1) != Some(value.new_revision)
            || value.deadline == 0
            || value.expires_at <= value.deadline
            || value.actions != ["native-memory.remember", "native-memory.recall"]
        {
            return Err(Error::Invalid);
        }
        // A configured UID is merely an expected value, not authentication.
        let _ = value.expected_publisher_uid;
        // These are pinned expectations; this loader never asserts that the
        // issuer is currently active or that any token family is unrevoked.
        let _ = (
            value.subject_version,
            value.client_version,
            value.resource_version,
        );
        Ok(value)
    }

    pub fn matches_hotel(&self, expected: &ExpectedHotelBinding<'_>) -> bool {
        self.hotel == expected.hotel
            && self.endpoint == expected.endpoint
            && self.owner == expected.owner
            && self.subject == expected.subject
            && self.client == expected.client
            && self.issuer == expected.issuer
            && self.resource == expected.resource
            && self.namespace == expected.namespace
            && self.runner == expected.runner
    }
}

/// Load only a root-owned disabled binding. The group must come from trusted
/// hotel configuration; this does not authenticate the expected publisher.
pub fn load_disabled_root_binding(path: &Path, group: u32) -> Result<DisabledOperationBinding> {
    DisabledOperationBinding::parse(&protected_config::read_with_owner(path, 0, group)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        os::unix::fs::{MetadataExt, PermissionsExt, symlink},
    };

    fn sample() -> serde_json::Value {
        serde_json::json!({
            "version":1,"enabled":false,"operation_id":"01234567-89ab-4cde-8fab-0123456789ab",
            "hotel":"hotel","endpoint":"pilot","owner":"owner","subject":"01234567-89ab-4cde-8fab-0123456789ab",
            "client":"client","issuer":"https://issuer.example","resource":"https://memory.example/mcp",
            "namespace":"pilot","runner":"runner","expected_publisher_service":"publisher.service",
            "expected_publisher_uid":0,"expected_publisher_executable_sha256":"a".repeat(64),
            "old_endpoint_sha256":"a".repeat(64),"new_endpoint_sha256":"b".repeat(64),
            "old_ingress_sha256":"c".repeat(64),"new_ingress_sha256":"d".repeat(64),
            "old_receiver_sha256":"e".repeat(64),"new_receiver_sha256":"f".repeat(64),
            "old_revision":2,"new_revision":3,"subject_version":1,"client_version":1,"resource_version":1,"deadline":100,"expires_at":200,
            "actions":["native-memory.remember","native-memory.recall"]
        })
    }
    fn fixture() -> (tempfile::TempDir, std::path::PathBuf, u32, u32) {
        let root = tempfile::Builder::new()
            .prefix("disabled-operation-")
            .tempdir_in(std::env::current_dir().unwrap())
            .unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o750)).unwrap();
        let path = root.path().join("operation.json");
        fs::write(&path, serde_json::to_vec(&sample()).unwrap()).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        let meta = fs::metadata(&path).unwrap();
        (root, path, meta.uid(), meta.gid())
    }
    fn read(path: &Path, uid: u32, gid: u32) -> Result<DisabledOperationBinding> {
        DisabledOperationBinding::parse(&protected_config::read_with_owner(path, uid, gid)?)
    }
    #[test]
    fn disabled_binding_requires_exact_expected_hotel() {
        let (_root, path, uid, gid) = fixture();
        let binding = read(&path, uid, gid).unwrap();
        let expected = ExpectedHotelBinding {
            hotel: "hotel",
            endpoint: "pilot",
            owner: "owner",
            subject: "01234567-89ab-4cde-8fab-0123456789ab",
            client: "client",
            issuer: "https://issuer.example",
            resource: "https://memory.example/mcp",
            namespace: "pilot",
            runner: "runner",
        };
        assert!(binding.matches_hotel(&expected));
        for mismatch in [
            ExpectedHotelBinding {
                hotel: "other",
                ..expected
            },
            ExpectedHotelBinding {
                endpoint: "other",
                ..expected
            },
            ExpectedHotelBinding {
                owner: "other",
                ..expected
            },
            ExpectedHotelBinding {
                subject: "other",
                ..expected
            },
            ExpectedHotelBinding {
                client: "other",
                ..expected
            },
            ExpectedHotelBinding {
                issuer: "other",
                ..expected
            },
            ExpectedHotelBinding {
                resource: "other",
                ..expected
            },
            ExpectedHotelBinding {
                namespace: "other",
                ..expected
            },
            ExpectedHotelBinding {
                runner: "other",
                ..expected
            },
        ] {
            assert!(!binding.matches_hotel(&mismatch));
        }
    }
    #[test]
    fn malformed_enabled_unknown_and_mismatched_revisions_fail_closed() {
        for (key, replacement) in [
            ("enabled", serde_json::json!(true)),
            ("version", serde_json::json!(2)),
            ("new_revision", serde_json::json!(4)),
            ("deadline", serde_json::json!(200)),
            ("new_ingress_sha256", serde_json::json!("c".repeat(64))),
            ("issuer", serde_json::json!("http://issuer.example")),
            ("actions", serde_json::json!(["*", "native-memory.recall"])),
        ] {
            let mut value = sample();
            value[key] = replacement;
            assert!(
                DisabledOperationBinding::parse(&serde_json::to_vec(&value).unwrap()).is_err(),
                "{key}"
            );
        }
        let mut value = sample();
        value["extra"] = serde_json::json!(true);
        assert!(DisabledOperationBinding::parse(&serde_json::to_vec(&value).unwrap()).is_err());
        let json = String::from_utf8(serde_json::to_vec(&sample()).unwrap()).unwrap();
        let duplicate = json.replacen(
            "\"enabled\":false",
            "\"enabled\":false,\"enabled\":false",
            1,
        );
        assert_ne!(json, duplicate);
        assert!(DisabledOperationBinding::parse(duplicate.as_bytes()).is_err());
        let mut missing = sample();
        missing.as_object_mut().unwrap().remove("operation_id");
        assert!(DisabledOperationBinding::parse(&serde_json::to_vec(&missing).unwrap()).is_err());
    }
    #[test]
    fn descriptor_reader_rejects_symlink_hardlink_and_writable_directory() {
        let (root, path, uid, gid) = fixture();
        let link = root.path().join("link");
        symlink(&path, &link).unwrap();
        assert!(read(&link, uid, gid).is_err());
        let alias = root.path().join("alias");
        fs::hard_link(&path, &alias).unwrap();
        assert!(read(&path, uid, gid).is_err());
        fs::remove_file(alias).unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o770)).unwrap();
        assert!(read(&path, uid, gid).is_err());
    }

    #[test]
    fn descriptor_reader_rejects_ancestor_alias_wrong_owner_and_oversize() {
        let (root, path, uid, gid) = fixture();
        if uid != 0 {
            assert!(load_disabled_root_binding(&path, gid).is_err());
        }
        let alias = root.path().with_extension("alias");
        symlink(root.path(), &alias).unwrap();
        assert!(read(&alias.join("operation.json"), uid, gid).is_err());
        fs::remove_file(alias).unwrap();
        assert!(read(&path, uid.wrapping_add(1), gid).is_err());
        assert!(read(&path, uid, gid.wrapping_add(1)).is_err());
        fs::write(&path, vec![b' '; 65537]).unwrap();
        assert!(read(&path, uid, gid).is_err());
    }
}
