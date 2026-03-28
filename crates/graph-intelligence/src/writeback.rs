use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context, Result};

/// Update specific YAML frontmatter fields in a markdown file.
///
/// Only the fields present in `updates` are changed; the rest of the
/// frontmatter and all content below the closing `---` are preserved
/// byte-for-byte.
pub fn update_frontmatter(
    file_path: &Path,
    updates: &HashMap<String, serde_yaml::Value>,
) -> Result<()> {
    let content = std::fs::read_to_string(file_path)
        .with_context(|| format!("Failed to read {}", file_path.display()))?;

    let updated = apply_frontmatter_updates(&content, updates)?;

    std::fs::write(file_path, &updated)
        .with_context(|| format!("Failed to write {}", file_path.display()))?;

    Ok(())
}

/// Update frontmatter fields and auto-commit the change.
pub fn update_and_commit(
    file_path: &Path,
    repo_root: &Path,
    updates: &HashMap<String, serde_yaml::Value>,
    agent: Option<&str>,
    session: Option<&str>,
    reason: &str,
) -> Result<()> {
    update_frontmatter(file_path, updates)?;

    let file_name = file_path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    let fields: Vec<&str> = updates.keys().map(|k| k.as_str()).collect();
    let fields_str = fields.join(", ");

    let mut message = format!("graph: update {} ({})", file_name, fields_str);
    if !reason.is_empty() {
        message.push_str(&format!("\n\n{}", reason));
    }
    if let Some(a) = agent {
        message.push_str(&format!("\nagent: {}", a));
    }
    if let Some(s) = session {
        message.push_str(&format!("\nsession: {}", s));
    }

    // git add
    let add_status = Command::new("git")
        .args(["add", &file_path.to_string_lossy()])
        .current_dir(repo_root)
        .status()
        .context("Failed to run git add")?;
    if !add_status.success() {
        bail!("git add failed for {}", file_path.display());
    }

    // git commit
    let commit_status = Command::new("git")
        .args(["commit", "-m", &message])
        .current_dir(repo_root)
        .status()
        .context("Failed to run git commit")?;
    if !commit_status.success() {
        bail!("git commit failed");
    }

    Ok(())
}

/// Parse frontmatter, apply updates, and reconstruct the file content.
fn apply_frontmatter_updates(
    content: &str,
    updates: &HashMap<String, serde_yaml::Value>,
) -> Result<String> {
    // Split on frontmatter delimiters
    let Some(rest) = content.strip_prefix("---") else {
        bail!("File does not start with frontmatter delimiter '---'");
    };

    let Some(end_idx) = rest.find("\n---") else {
        bail!("Could not find closing frontmatter delimiter '---'");
    };

    let yaml_str = &rest[..end_idx + 1]; // include trailing newline
    let after_closing = &rest[end_idx + 4..]; // skip "\n---"

    // Parse the YAML frontmatter
    let mut yaml: serde_yaml::Value =
        serde_yaml::from_str(yaml_str).context("Failed to parse YAML frontmatter")?;

    // Apply updates to the YAML mapping
    if let serde_yaml::Value::Mapping(ref mut map) = yaml {
        for (key, value) in updates {
            map.insert(
                serde_yaml::Value::String(key.clone()),
                value.clone(),
            );
        }
    } else {
        bail!("Frontmatter is not a YAML mapping");
    }

    // Re-serialize the YAML
    let updated_yaml = serde_yaml::to_string(&yaml).context("Failed to serialize YAML")?;

    // Reconstruct: ---\n{yaml}---\n{rest}
    // serde_yaml::to_string already ends with \n
    let mut result = String::with_capacity(content.len());
    result.push_str("---\n");
    result.push_str(&updated_yaml);
    result.push_str("---");
    result.push_str(after_closing);

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_apply_frontmatter_updates() {
        let content = "---\ntitle: Test Proposal\nstatus: draft\ndomain: core\n---\n\n# Content\n\nThis is the body.\n";

        let mut updates = HashMap::new();
        updates.insert(
            "status".to_string(),
            serde_yaml::Value::String("accepted".to_string()),
        );

        let result = apply_frontmatter_updates(content, &updates).unwrap();

        assert!(result.starts_with("---\n"));
        assert!(result.contains("status: accepted"));
        assert!(result.contains("title: Test Proposal"));
        assert!(result.contains("domain: core"));
        assert!(result.contains("# Content"));
        assert!(result.contains("This is the body."));
    }

    #[test]
    fn test_preserves_content_below_frontmatter() {
        let body = "\n\n# Big Section\n\nLots of content here.\n\n## Sub-section\n\nMore content.\n";
        let content = format!("---\nkey: value\n---{}", body);

        let mut updates = HashMap::new();
        updates.insert(
            "key".to_string(),
            serde_yaml::Value::String("new_value".to_string()),
        );

        let result = apply_frontmatter_updates(&content, &updates).unwrap();

        assert!(result.ends_with(body), "Body was not preserved. Got: {}", result);
    }

    #[test]
    fn test_adds_new_field() {
        let content = "---\ntitle: Test\n---\n\nBody\n";

        let mut updates = HashMap::new();
        updates.insert(
            "new_field".to_string(),
            serde_yaml::Value::String("new_value".to_string()),
        );

        let result = apply_frontmatter_updates(content, &updates).unwrap();

        assert!(result.contains("new_field: new_value"));
        assert!(result.contains("title: Test"));
    }
}
