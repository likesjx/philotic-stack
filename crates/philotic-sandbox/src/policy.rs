use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum PolicyError {
    #[error("failed to read policy file: {0}")]
    Io(#[from] std::io::Error),
    #[error("failed to parse policy TOML: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("policy validation failed: {0}")]
    Validation(String),
}

/// Top-level sandbox policy parsed from TOML.
#[derive(Debug, Clone, Deserialize)]
pub struct SandboxPolicy {
    pub sandbox: SandboxMeta,
    #[serde(default)]
    pub commands: CommandPolicy,
    #[serde(default)]
    pub filesystem: FilesystemPolicy,
    #[serde(default)]
    pub environment: EnvironmentPolicy,
    #[serde(default)]
    pub network: NetworkPolicy,
    #[serde(default)]
    pub seccomp: SeccompPolicy,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SandboxMeta {
    pub name: String,
    #[serde(default = "default_version")]
    pub version: String,
    #[serde(default = "default_max_timeout_ms")]
    pub max_timeout_ms: u64,
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent: usize,
}

fn default_version() -> String {
    "1.0".to_string()
}

fn default_max_timeout_ms() -> u64 {
    30_000
}

fn default_max_concurrent() -> usize {
    4
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct CommandPolicy {
    #[serde(default)]
    pub allowed: Vec<String>,
    #[serde(default)]
    pub denied: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct FilesystemPolicy {
    #[serde(default)]
    pub read_paths: Vec<String>,
    #[serde(default)]
    pub write_paths: Vec<String>,
    #[serde(default)]
    pub execute_paths: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct EnvironmentPolicy {
    #[serde(default)]
    pub allowlist: Vec<String>,
    #[serde(default)]
    pub forced: HashMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct NetworkPolicy {
    #[serde(default = "default_network_mode")]
    pub mode: String,
    #[serde(default)]
    pub allowed_hosts: Vec<String>,
}

impl Default for NetworkPolicy {
    fn default() -> Self {
        Self {
            mode: default_network_mode(),
            allowed_hosts: Vec::new(),
        }
    }
}

fn default_network_mode() -> String {
    "none".to_string()
}

#[derive(Debug, Clone, Deserialize)]
pub struct SeccompPolicy {
    #[serde(default = "default_seccomp_profile")]
    pub profile: String,
}

impl Default for SeccompPolicy {
    fn default() -> Self {
        Self {
            profile: default_seccomp_profile(),
        }
    }
}

fn default_seccomp_profile() -> String {
    "shell_executor".to_string()
}

impl SandboxPolicy {
    /// Load a policy from a TOML file.
    pub fn from_file(path: &Path) -> Result<Self, PolicyError> {
        let contents = std::fs::read_to_string(path)?;
        Self::from_str(&contents)
    }

    /// Parse a policy from a TOML string.
    pub fn from_str(s: &str) -> Result<Self, PolicyError> {
        let policy: SandboxPolicy = toml::from_str(s)?;
        policy.validate()?;
        Ok(policy)
    }

    /// Validate policy constraints.
    pub fn validate(&self) -> Result<(), PolicyError> {
        if self.sandbox.name.is_empty() {
            return Err(PolicyError::Validation(
                "sandbox.name must not be empty".into(),
            ));
        }

        if self.sandbox.max_timeout_ms == 0 {
            return Err(PolicyError::Validation(
                "sandbox.max_timeout_ms must be > 0".into(),
            ));
        }

        if self.sandbox.max_concurrent == 0 {
            return Err(PolicyError::Validation(
                "sandbox.max_concurrent must be > 0".into(),
            ));
        }

        let valid_network_modes = ["none", "allowlist", "unrestricted"];
        if !valid_network_modes.contains(&self.network.mode.as_str()) {
            return Err(PolicyError::Validation(format!(
                "network.mode must be one of {:?}, got '{}'",
                valid_network_modes, self.network.mode
            )));
        }

        let valid_profiles = [
            "strict",
            "shell_executor",
            "file_processor",
            "network_server",
        ];
        if !valid_profiles.contains(&self.seccomp.profile.as_str()) {
            return Err(PolicyError::Validation(format!(
                "seccomp.profile must be one of {:?}, got '{}'",
                valid_profiles, self.seccomp.profile
            )));
        }

        Ok(())
    }

    /// Check if a command is allowed by the policy.
    pub fn is_command_allowed(&self, command: &str) -> bool {
        // Denied list takes priority
        for denied in &self.commands.denied {
            if command == denied || glob_match(denied, command) {
                return false;
            }
        }

        // Check allowed list
        for allowed in &self.commands.allowed {
            if command == allowed || glob_match(allowed, command) {
                return true;
            }
        }

        false
    }

    /// Check if a path is allowed for reading.
    pub fn is_read_path_allowed(&self, path: &str) -> bool {
        self.filesystem
            .read_paths
            .iter()
            .any(|pattern| path_matches(pattern, path))
    }

    /// Check if a path is allowed for writing.
    pub fn is_write_path_allowed(&self, path: &str) -> bool {
        self.filesystem
            .write_paths
            .iter()
            .any(|pattern| path_matches(pattern, path))
    }

    /// Filter environment variables: keep only allowlisted and merge forced.
    pub fn filter_env(&self, env: &HashMap<String, String>) -> HashMap<String, String> {
        let mut filtered: HashMap<String, String> = env
            .iter()
            .filter(|(k, _)| self.environment.allowlist.iter().any(|a| a == *k))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();

        // Forced vars override everything
        for (k, v) in &self.environment.forced {
            filtered.insert(k.clone(), v.clone());
        }

        filtered
    }

    /// Get the effective timeout: minimum of requested and policy max.
    pub fn effective_timeout_ms(&self, requested: Option<u64>) -> u64 {
        match requested {
            Some(t) => t.min(self.sandbox.max_timeout_ms),
            None => self.sandbox.max_timeout_ms,
        }
    }
}

/// Simple glob matching for command names.
fn glob_match(pattern: &str, value: &str) -> bool {
    glob::Pattern::new(pattern)
        .map(|p| p.matches(value))
        .unwrap_or(false)
}

/// Check if a path matches a glob pattern, handling `**` for recursive matching.
fn path_matches(pattern: &str, path: &str) -> bool {
    let opts = glob::MatchOptions {
        case_sensitive: true,
        require_literal_separator: false,
        require_literal_leading_dot: false,
    };
    glob::Pattern::new(pattern)
        .map(|p| p.matches_with(path, opts))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID_POLICY: &str = r#"
[sandbox]
name = "test-policy"
version = "1.0"
max_timeout_ms = 10000
max_concurrent = 2

[commands]
allowed = ["ls", "cat", "grep"]
denied = ["sudo", "rm"]

[filesystem]
read_paths = ["/tmp/**", "./workspace/**"]
write_paths = ["./workspace/output/**"]
execute_paths = ["/usr/bin/**"]

[environment]
allowlist = ["PATH", "HOME"]
forced = { SANDBOX = "1" }

[network]
mode = "none"

[seccomp]
profile = "shell_executor"
"#;

    #[test]
    fn parse_valid_policy() {
        let policy = SandboxPolicy::from_str(VALID_POLICY).unwrap();
        assert_eq!(policy.sandbox.name, "test-policy");
        assert_eq!(policy.sandbox.max_timeout_ms, 10000);
        assert_eq!(policy.sandbox.max_concurrent, 2);
        assert_eq!(policy.commands.allowed.len(), 3);
        assert_eq!(policy.commands.denied.len(), 2);
    }

    #[test]
    fn parse_minimal_policy() {
        let toml = r#"
[sandbox]
name = "minimal"
"#;
        let policy = SandboxPolicy::from_str(toml).unwrap();
        assert_eq!(policy.sandbox.name, "minimal");
        assert_eq!(policy.sandbox.max_timeout_ms, 30_000);
        assert_eq!(policy.sandbox.max_concurrent, 4);
        assert_eq!(policy.network.mode, "none");
        assert_eq!(policy.seccomp.profile, "shell_executor");
    }

    #[test]
    fn reject_empty_name() {
        let toml = r#"
[sandbox]
name = ""
"#;
        let err = SandboxPolicy::from_str(toml).unwrap_err();
        assert!(err.to_string().contains("name must not be empty"));
    }

    #[test]
    fn reject_invalid_network_mode() {
        let toml = r#"
[sandbox]
name = "test"

[network]
mode = "invalid"
"#;
        let err = SandboxPolicy::from_str(toml).unwrap_err();
        assert!(err.to_string().contains("network.mode"));
    }

    #[test]
    fn reject_invalid_seccomp_profile() {
        let toml = r#"
[sandbox]
name = "test"

[seccomp]
profile = "bogus"
"#;
        let err = SandboxPolicy::from_str(toml).unwrap_err();
        assert!(err.to_string().contains("seccomp.profile"));
    }

    #[test]
    fn reject_zero_timeout() {
        let toml = r#"
[sandbox]
name = "test"
max_timeout_ms = 0
"#;
        let err = SandboxPolicy::from_str(toml).unwrap_err();
        assert!(err.to_string().contains("max_timeout_ms"));
    }

    #[test]
    fn command_allowed_check() {
        let policy = SandboxPolicy::from_str(VALID_POLICY).unwrap();
        assert!(policy.is_command_allowed("ls"));
        assert!(policy.is_command_allowed("cat"));
        assert!(!policy.is_command_allowed("sudo"));
        assert!(!policy.is_command_allowed("rm"));
        assert!(!policy.is_command_allowed("wget"));
    }

    #[test]
    fn path_allowed_check() {
        let policy = SandboxPolicy::from_str(VALID_POLICY).unwrap();
        assert!(policy.is_read_path_allowed("/tmp/foo"));
        assert!(policy.is_read_path_allowed("./workspace/src/main.rs"));
        assert!(!policy.is_read_path_allowed("/etc/passwd"));
    }

    #[test]
    fn env_filtering() {
        let policy = SandboxPolicy::from_str(VALID_POLICY).unwrap();
        let mut env = HashMap::new();
        env.insert("PATH".into(), "/usr/bin".into());
        env.insert("HOME".into(), "/home/user".into());
        env.insert("SECRET".into(), "should_be_removed".into());

        let filtered = policy.filter_env(&env);
        assert_eq!(filtered.get("PATH").unwrap(), "/usr/bin");
        assert_eq!(filtered.get("HOME").unwrap(), "/home/user");
        assert!(!filtered.contains_key("SECRET"));
        assert_eq!(filtered.get("SANDBOX").unwrap(), "1");
    }

    #[test]
    fn effective_timeout() {
        let policy = SandboxPolicy::from_str(VALID_POLICY).unwrap();
        assert_eq!(policy.effective_timeout_ms(Some(5000)), 5000);
        assert_eq!(policy.effective_timeout_ms(Some(60000)), 10000);
        assert_eq!(policy.effective_timeout_ms(None), 10000);
    }

    #[test]
    fn reject_missing_sandbox_section() {
        let toml = r#"
[commands]
allowed = ["ls"]
"#;
        assert!(SandboxPolicy::from_str(toml).is_err());
    }
}
