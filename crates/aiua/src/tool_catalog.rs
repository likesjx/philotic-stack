//! The tool catalog file: `catalog/tools.yaml` → `abstract_tool` records.
//!
//! Tool definitions used to be compiled into three separate Rust tables (the
//! philote's `catalog.rs`, this crate's `seed_abstract_tool_catalog`, and the
//! LifeGraph runner's tool list) that had drifted apart: 108 vs 49 tools, 36
//! shared, and only 3 shared descriptions identical. The operator's rule is
//! that tools are data, not code. This module is the single loader.
//!
//! Resolution order, merged by `tool_name` (a later source replaces the whole
//! entry):
//! 1. the file compiled into this binary — the version-controlled floor, so a
//!    hotel always has a complete catalog even with no files on disk;
//! 2. `~/.philotic/<profile>/tool-catalog.yaml`, when it exists;
//! 3. the file named by `PHILOTIC_TOOL_CATALOG`, when set.
//!
//! An override file uses the same format and may carry only the tools it
//! changes (plus any `defs` those entries reference).

use std::collections::BTreeMap;
#[cfg(test)]
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use ansible_mesh_core::graph::{AbstractToolRecord, ToolBatchOf};
use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use serde_json::Value;

/// The version-controlled catalog, compiled in as the floor.
pub const EMBEDDED_TOOL_CATALOG: &str = include_str!("../../../catalog/tools.yaml");

const MAX_REF_DEPTH: usize = 16;

#[derive(Debug, Deserialize)]
struct CatalogFile {
    #[serde(default)]
    version: Option<u32>,
    #[serde(default)]
    defs: BTreeMap<String, Value>,
    #[serde(default)]
    tools: Vec<CatalogTool>,
}

#[derive(Debug, Deserialize)]
struct CatalogTool {
    tool_name: String,
    class: String,
    description: String,
    #[serde(default)]
    tool_markers: Vec<String>,
    #[serde(default)]
    batch_of: Option<ToolBatchOf>,
    #[serde(default)]
    input_schema: Option<Value>,
}

/// Parse one catalog document into records, resolving `{"$ref": "#/defs/x"}`
/// against the document's own `defs` (plus `inherited_defs`, so an override
/// file can reference fragments defined in the floor).
pub fn parse_tool_catalog(
    source_label: &str,
    yaml: &str,
    inherited_defs: &BTreeMap<String, Value>,
) -> Result<(Vec<AbstractToolRecord>, BTreeMap<String, Value>)> {
    let file: CatalogFile = serde_yaml::from_str(yaml)
        .with_context(|| format!("tool catalog {source_label}: not valid catalog YAML"))?;
    if let Some(version) = file.version
        && version != 1
    {
        bail!("tool catalog {source_label}: unsupported version {version} (expected 1)");
    }
    let mut defs = inherited_defs.clone();
    defs.extend(file.defs);

    let mut seen = std::collections::BTreeSet::new();
    let mut records = Vec::with_capacity(file.tools.len());
    for tool in file.tools {
        let name = tool.tool_name.trim().to_string();
        if name.is_empty() {
            bail!("tool catalog {source_label}: a tool has an empty tool_name");
        }
        if !seen.insert(name.clone()) {
            bail!("tool catalog {source_label}: duplicate tool_name `{name}`");
        }
        if tool.description.trim().is_empty() {
            bail!("tool catalog {source_label}: `{name}` has an empty description");
        }
        if tool.class.trim().is_empty() {
            bail!("tool catalog {source_label}: `{name}` has an empty class");
        }
        let schema = tool
            .input_schema
            .unwrap_or_else(|| serde_json::json!({ "type": "object" }));
        let input_schema = resolve_refs(&schema, &defs, 0)
            .with_context(|| format!("tool catalog {source_label}: `{name}` input_schema"))?;
        if let Some(batch) = &tool.batch_of
            && (batch.tool.trim().is_empty() || !batch.items_pointer.starts_with('/'))
        {
            bail!(
                "tool catalog {source_label}: `{name}` batch_of needs a tool and an \
                 items_pointer starting with '/'"
            );
        }
        records.push(AbstractToolRecord {
            tool_name: name,
            description: tool.description,
            input_schema,
            class: tool.class,
            tool_markers: tool.tool_markers,
            batch_of: tool.batch_of,
        });
    }
    Ok((records, defs))
}

fn resolve_refs(node: &Value, defs: &BTreeMap<String, Value>, depth: usize) -> Result<Value> {
    if depth > MAX_REF_DEPTH {
        bail!("$ref nesting deeper than {MAX_REF_DEPTH} (cycle?)");
    }
    match node {
        Value::Object(map) => {
            if map.len() == 1
                && let Some(Value::String(target)) = map.get("$ref")
            {
                let key = target
                    .strip_prefix("#/defs/")
                    .ok_or_else(|| anyhow!("unsupported $ref `{target}` (use #/defs/<name>)"))?;
                let def = defs
                    .get(key)
                    .ok_or_else(|| anyhow!("$ref `{target}` names no entry in defs"))?;
                return resolve_refs(def, defs, depth + 1);
            }
            let mut out = serde_json::Map::with_capacity(map.len());
            for (k, v) in map {
                out.insert(k.clone(), resolve_refs(v, defs, depth)?);
            }
            Ok(Value::Object(out))
        }
        Value::Array(items) => Ok(Value::Array(
            items
                .iter()
                .map(|v| resolve_refs(v, defs, depth))
                .collect::<Result<Vec<_>>>()?,
        )),
        other => Ok(other.clone()),
    }
}

/// Merge layers by tool_name, later layers replacing earlier entries, then
/// check every `batch_of.tool` names a tool in the merged catalog.
pub fn merge_catalog_layers(
    layers: Vec<Vec<AbstractToolRecord>>,
) -> Result<Vec<AbstractToolRecord>> {
    let mut merged: BTreeMap<String, AbstractToolRecord> = BTreeMap::new();
    for layer in layers {
        for record in layer {
            merged.insert(record.tool_name.clone(), record);
        }
    }
    let names: std::collections::BTreeSet<&str> = merged.keys().map(String::as_str).collect();
    for record in merged.values() {
        if let Some(batch) = &record.batch_of
            && !names.contains(batch.tool.as_str())
        {
            bail!(
                "tool catalog: `{}` is batch_of `{}`, which is not in the catalog",
                record.tool_name,
                batch.tool
            );
        }
    }
    Ok(merged.into_values().collect())
}

/// The override files that apply to this process, in resolution order.
pub fn override_paths(profile_dir: Option<&Path>) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(dir) = profile_dir {
        paths.push(dir.join("tool-catalog.yaml"));
    }
    if let Ok(explicit) = std::env::var("PHILOTIC_TOOL_CATALOG")
        && !explicit.trim().is_empty()
    {
        paths.push(PathBuf::from(explicit));
    }
    paths
}

/// Load the embedded floor plus every existing override file.
///
/// The floor must parse (it is compiled in and covered by tests). An override
/// that fails to parse, or whose merge breaks a `batch_of` target, is SKIPPED
/// and reported in the returned errors rather than failing the hotel boot — a
/// typo in an operator's edit must not take every agent down. The caller logs
/// the errors loudly.
pub fn load_tool_catalog(profile_dir: Option<&Path>) -> Result<LoadedToolCatalog> {
    let floor_label = "embedded catalog/tools.yaml";
    let (floor, floor_defs) =
        parse_tool_catalog(floor_label, EMBEDDED_TOOL_CATALOG, &BTreeMap::new())?;
    let mut layers = vec![floor];
    let mut sources = vec![floor_label.to_string()];
    let mut errors = Vec::new();
    let explicit = std::env::var("PHILOTIC_TOOL_CATALOG")
        .ok()
        .filter(|p| !p.trim().is_empty());
    for path in override_paths(profile_dir) {
        let label = path.display().to_string();
        let is_explicit = explicit.as_deref() == Some(label.as_str());
        if !path.exists() {
            if is_explicit {
                errors.push(format!(
                    "PHILOTIC_TOOL_CATALOG names {label}, which does not exist"
                ));
            }
            continue;
        }
        let parsed = std::fs::read_to_string(&path)
            .with_context(|| format!("tool catalog {label}: unreadable"))
            .and_then(|yaml| parse_tool_catalog(&label, &yaml, &floor_defs));
        match parsed {
            Ok((records, _)) => {
                let mut candidate = layers.clone();
                candidate.push(records.clone());
                match merge_catalog_layers(candidate) {
                    Ok(_) => {
                        layers.push(records);
                        sources.push(label);
                    }
                    Err(err) => errors.push(format!("{err:#} (override {label} skipped)")),
                }
            }
            Err(err) => errors.push(format!("{err:#} (override skipped)")),
        }
    }
    Ok(LoadedToolCatalog {
        records: merge_catalog_layers(layers)?,
        sources,
        errors,
    })
}

/// The merged catalog plus where it came from and any skipped overrides.
pub struct LoadedToolCatalog {
    pub records: Vec<AbstractToolRecord>,
    pub sources: Vec<String>,
    pub errors: Vec<String>,
}

/// Index a loaded catalog by name (tests).
#[cfg(test)]
pub fn index_by_name(records: &[AbstractToolRecord]) -> HashMap<&str, &AbstractToolRecord> {
    records.iter().map(|r| (r.tool_name.as_str(), r)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_catalog_parses_and_declares_the_observe_batch_relationship() {
        let (records, defs) =
            parse_tool_catalog("embedded", EMBEDDED_TOOL_CATALOG, &BTreeMap::new()).expect("parse");
        assert!(records.len() >= 121, "catalog shrank: {}", records.len());
        assert!(defs.contains_key("evidence_packet"));
        let merged = merge_catalog_layers(vec![records]).expect("batch targets resolve");
        let by_name = index_by_name(&merged);
        let batch = by_name["life.observe.batch"]
            .batch_of
            .as_ref()
            .expect("batch_of");
        assert_eq!(batch.tool, "life.observe");
        assert_eq!(batch.items_pointer, "/observations");
        // $refs are resolved at load: no record carries a raw reference.
        for record in &merged {
            assert!(
                !record.input_schema.to_string().contains("\"$ref\""),
                "{} still has an unresolved $ref",
                record.tool_name
            );
        }
        // The 13 tools only the old hotel seed defined survive the migration.
        for name in [
            "asr.setup",
            "vision.status",
            "training.export",
            "hotel.egress.check",
            "image.ocr",
        ] {
            assert!(by_name.contains_key(name), "{name} lost in migration");
        }
        // And the old hotel seed's markers survive too.
        assert!(
            by_name["bash.exec"]
                .tool_markers
                .contains(&"high_agency".to_string())
        );
    }

    #[test]
    fn override_layer_replaces_by_name_and_can_use_floor_defs() {
        let (floor, floor_defs) =
            parse_tool_catalog("embedded", EMBEDDED_TOOL_CATALOG, &BTreeMap::new()).unwrap();
        let yaml = r##"
version: 1
tools:
  - tool_name: echo
    class: utility
    description: "Operator-edited echo description."
    input_schema: { type: object, properties: { text: { type: string } } }
  - tool_name: custom.tool
    class: utility
    description: "A tool that only the override defines."
    input_schema:
      type: object
      properties:
        ref: { $ref: "#/defs/graph_record_ref" }
"##;
        let (layer, _) =
            parse_tool_catalog("override", yaml, &floor_defs).expect("override parses");
        let merged = merge_catalog_layers(vec![floor, layer]).unwrap();
        let by_name = index_by_name(&merged);
        assert_eq!(
            by_name["echo"].description,
            "Operator-edited echo description."
        );
        assert!(by_name["custom.tool"].input_schema["properties"]["ref"].is_object());
        assert!(
            by_name.contains_key("life.observe"),
            "untouched floor entries remain"
        );
    }

    #[test]
    fn a_broken_profile_override_is_skipped_not_fatal() {
        let dir = std::env::temp_dir().join(format!("toolcat-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("tool-catalog.yaml"), "tools: [ this is: not valid").unwrap();
        let loaded = load_tool_catalog(Some(&dir)).expect("floor still loads");
        assert!(loaded.records.len() >= 121);
        assert_eq!(loaded.sources.len(), 1, "broken override is not a source");
        assert_eq!(loaded.errors.len(), 1, "{:?}", loaded.errors);
        std::fs::write(
            dir.join("tool-catalog.yaml"),
            "tools:\n  - {tool_name: echo, class: utility, description: Edited echo.}\n",
        )
        .unwrap();
        let loaded = load_tool_catalog(Some(&dir)).unwrap();
        assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);
        let echo = loaded
            .records
            .iter()
            .find(|r| r.tool_name == "echo")
            .unwrap();
        assert_eq!(echo.description, "Edited echo.");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn malformed_catalogs_are_refused_with_the_reason() {
        let dup = "tools:\n  - {tool_name: a, class: x, description: d}\n  - {tool_name: a, class: x, description: d}\n";
        let err = parse_tool_catalog("t", dup, &BTreeMap::new())
            .unwrap_err()
            .to_string();
        assert!(err.contains("duplicate tool_name `a`"), "{err}");

        let bad_ref = "tools:\n  - {tool_name: a, class: x, description: d, input_schema: {$ref: '#/defs/nope'}}\n";
        let err = format!(
            "{:#}",
            parse_tool_catalog("t", bad_ref, &BTreeMap::new()).unwrap_err()
        );
        assert!(err.contains("names no entry in defs"), "{err}");

        let dangling = "tools:\n  - {tool_name: a.batch, class: x, description: d, batch_of: {tool: a, items_pointer: /items}}\n";
        let (records, _) = parse_tool_catalog("t", dangling, &BTreeMap::new()).unwrap();
        let err = merge_catalog_layers(vec![records]).unwrap_err().to_string();
        assert!(err.contains("not in the catalog"), "{err}");
    }
}
