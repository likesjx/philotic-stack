use std::collections::HashMap;

use graph_intelligence::writeback;

#[test]
fn test_update_frontmatter_changes_status() {
    let dir = tempfile::tempdir().expect("Failed to create temp dir");
    let file_path = dir.path().join("test-proposal.md");

    let original = "---\ntitle: Test Proposal\nstatus: draft\ndomain: core\ntags:\n  - architecture\n  - phase-1\n---\n\n# Test Proposal\n\nThis is the body content.\n\n## Details\n\nMore content here.\n";
    std::fs::write(&file_path, original).expect("Failed to write temp file");

    let mut updates = HashMap::new();
    updates.insert(
        "status".to_string(),
        serde_yaml::Value::String("accepted".to_string()),
    );

    writeback::update_frontmatter(&file_path, &updates).expect("Failed to update frontmatter");

    let result = std::fs::read_to_string(&file_path).expect("Failed to read result");

    // Status should be updated
    assert!(
        result.contains("status: accepted"),
        "Status not updated. Got: {}",
        result
    );
    // Other fields preserved
    assert!(result.contains("title: Test Proposal"));
    assert!(result.contains("domain: core"));
    // Body content preserved
    assert!(result.contains("# Test Proposal"));
    assert!(result.contains("This is the body content."));
    assert!(result.contains("More content here."));
    // File starts with frontmatter
    assert!(result.starts_with("---\n"));
}

#[test]
fn test_update_frontmatter_preserves_body_exactly() {
    let dir = tempfile::tempdir().expect("Failed to create temp dir");
    let file_path = dir.path().join("preserve-test.md");

    let body = "\n\n# Section\n\nParagraph with special chars: <>&\"'\n\n```rust\nfn main() {\n    println!(\"hello\");\n}\n```\n\n- item 1\n- item 2\n";
    let original = format!("---\nkey: value\n---{}", body);
    std::fs::write(&file_path, &original).expect("Failed to write temp file");

    let mut updates = HashMap::new();
    updates.insert(
        "key".to_string(),
        serde_yaml::Value::String("new_value".to_string()),
    );

    writeback::update_frontmatter(&file_path, &updates).expect("Failed to update frontmatter");

    let result = std::fs::read_to_string(&file_path).expect("Failed to read result");

    // Body must be preserved exactly
    assert!(
        result.ends_with(body),
        "Body was not preserved.\nExpected to end with: {:?}\nGot: {:?}",
        body,
        result
    );
}

#[test]
fn test_update_frontmatter_adds_new_field() {
    let dir = tempfile::tempdir().expect("Failed to create temp dir");
    let file_path = dir.path().join("add-field.md");

    let original = "---\ntitle: Test\n---\n\nBody\n";
    std::fs::write(&file_path, original).expect("Failed to write temp file");

    let mut updates = HashMap::new();
    updates.insert(
        "last_updated".to_string(),
        serde_yaml::Value::String("2026-03-28".to_string()),
    );

    writeback::update_frontmatter(&file_path, &updates).expect("Failed to update frontmatter");

    let result = std::fs::read_to_string(&file_path).expect("Failed to read result");

    assert!(result.contains("last_updated: '2026-03-28'") || result.contains("last_updated: 2026-03-28"));
    assert!(result.contains("title: Test"));
    assert!(result.contains("Body"));
}

#[test]
fn test_update_frontmatter_with_list_values() {
    let dir = tempfile::tempdir().expect("Failed to create temp dir");
    let file_path = dir.path().join("list-test.md");

    let original = "---\ntitle: Test\nimplements:\n  - SEAM-001\n---\n\nBody\n";
    std::fs::write(&file_path, original).expect("Failed to write temp file");

    let mut updates = HashMap::new();
    let new_list = serde_yaml::Value::Sequence(vec![
        serde_yaml::Value::String("SEAM-001".to_string()),
        serde_yaml::Value::String("SEAM-002".to_string()),
    ]);
    updates.insert("implements".to_string(), new_list);

    writeback::update_frontmatter(&file_path, &updates).expect("Failed to update frontmatter");

    let result = std::fs::read_to_string(&file_path).expect("Failed to read result");

    assert!(result.contains("SEAM-001"));
    assert!(result.contains("SEAM-002"));
    assert!(result.contains("title: Test"));
}
