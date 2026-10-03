//! The `philotic.desktop.v1` catalog, loaded from the vendored JSON file that the
//! web renderer also reads, so the allowlist has one source and two enforcers.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use serde_json::Value;

/// The catalog file, compiled in.
pub const CATALOG_JSON: &str = include_str!("../../specs/a2ui/philotic_desktop_v1.json");

/// How a component property is checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PropKind {
    DynamicString,
    DynamicNumber,
    DynamicBoolean,
    DynamicStringList,
    ComponentId,
    ChildList,
    Action,
    LiteralString,
    LiteralNumber,
    LiteralBoolean,
    Options,
    Checks,
    TableColumns,
    TableRows,
    Accessibility,
    Enum(Vec<String>),
}

impl PropKind {
    fn parse(value: &Value) -> Result<Self, String> {
        if let Some(values) = value.get("enum").and_then(Value::as_array) {
            let values = values
                .iter()
                .map(|v| v.as_str().map(str::to_string))
                .collect::<Option<Vec<_>>>()
                .ok_or("enum values must be strings")?;
            return Ok(Self::Enum(values));
        }
        Ok(
            match value.as_str().ok_or("prop kind must be a string or enum")? {
                "dynamicString" => Self::DynamicString,
                "dynamicNumber" => Self::DynamicNumber,
                "dynamicBoolean" => Self::DynamicBoolean,
                "dynamicStringList" => Self::DynamicStringList,
                "componentId" => Self::ComponentId,
                "childList" => Self::ChildList,
                "action" => Self::Action,
                "literalString" => Self::LiteralString,
                "literalNumber" => Self::LiteralNumber,
                "literalBoolean" => Self::LiteralBoolean,
                "options" => Self::Options,
                "checks" => Self::Checks,
                "tableColumns" => Self::TableColumns,
                "tableRows" => Self::TableRows,
                "accessibility" => Self::Accessibility,
                other => return Err(format!("unknown prop kind {other}")),
            },
        )
    }
}

#[derive(Debug, Clone)]
pub struct ComponentSpec {
    pub required: Vec<String>,
    pub props: BTreeMap<String, PropKind>,
}

#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub max_components: usize,
    pub max_depth: usize,
    pub max_message_bytes: usize,
    pub max_data_model_bytes: usize,
    pub max_string_bytes: usize,
    pub max_regex_bytes: usize,
}

#[derive(Debug, Clone)]
pub struct Catalog {
    pub catalog_id: String,
    pub a2ui_version: String,
    pub common_props: BTreeMap<String, PropKind>,
    pub components: BTreeMap<String, ComponentSpec>,
    pub excluded: Vec<String>,
    pub limits: Limits,
}

impl Catalog {
    fn parse(text: &str) -> Result<Self, String> {
        let root: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
        let string = |key: &str| {
            root.get(key)
                .and_then(Value::as_str)
                .map(str::to_string)
                .ok_or(format!("catalog missing {key}"))
        };
        let props = |value: Option<&Value>| -> Result<BTreeMap<String, PropKind>, String> {
            value
                .and_then(Value::as_object)
                .map(|map| {
                    map.iter()
                        .map(|(k, v)| PropKind::parse(v).map(|kind| (k.clone(), kind)))
                        .collect()
                })
                .unwrap_or_else(|| Ok(BTreeMap::new()))
        };
        let mut components = BTreeMap::new();
        for (name, spec) in root
            .get("components")
            .and_then(Value::as_object)
            .ok_or("catalog missing components")?
        {
            let required = spec
                .get("required")
                .and_then(Value::as_array)
                .map(|r| {
                    r.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            components.insert(
                name.clone(),
                ComponentSpec {
                    required,
                    props: props(spec.get("props"))?,
                },
            );
        }
        let limits = root.get("limits").ok_or("catalog missing limits")?;
        let limit = |key: &str| {
            limits
                .get(key)
                .and_then(Value::as_u64)
                .map(|v| v as usize)
                .ok_or(format!("catalog missing limit {key}"))
        };
        Ok(Self {
            catalog_id: string("catalogId")?,
            a2ui_version: string("a2uiVersion")?,
            common_props: props(root.get("commonProps"))?,
            components,
            excluded: root
                .get("excluded")
                .and_then(Value::as_object)
                .map(|m| m.keys().cloned().collect())
                .unwrap_or_default(),
            limits: Limits {
                max_components: limit("maxComponents")?,
                max_depth: limit("maxDepth")?,
                max_message_bytes: limit("maxMessageBytes")?,
                max_data_model_bytes: limit("maxDataModelBytes")?,
                max_string_bytes: limit("maxStringBytes")?,
                max_regex_bytes: limit("maxRegexBytes")?,
            },
        })
    }
}

/// The compiled-in catalog. Panics only if the vendored file is malformed, which
/// the unit tests below catch before anything ships.
pub fn catalog() -> &'static Catalog {
    static CATALOG: OnceLock<Catalog> = OnceLock::new();
    CATALOG.get_or_init(|| Catalog::parse(CATALOG_JSON).expect("philotic_desktop_v1.json is valid"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_parses_with_expected_identity() {
        let c = catalog();
        assert_eq!(c.catalog_id, "philotic.desktop.v1");
        assert_eq!(c.a2ui_version, "v0.9");
        assert!(c.limits.max_components > 0);
    }

    /// Every allowed component except the Philotic `Table` extension must exist in
    /// the vendored A2UI basic catalog, with the same required props, so the
    /// subset never drifts from the upstream contract.
    #[test]
    fn allowed_components_are_a_subset_of_the_basic_catalog() {
        let basic: Value =
            serde_json::from_str(include_str!("../../specs/a2ui/v0_9/basic_catalog.json")).unwrap();
        let basic = basic["components"].as_object().unwrap();
        for (name, spec) in &catalog().components {
            if name == "Table" {
                continue;
            }
            let upstream = basic
                .get(name)
                .unwrap_or_else(|| panic!("{name} is not in the A2UI basic catalog"));
            let mut upstream_required: Vec<String> = upstream["allOf"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|part| part.get("required").and_then(Value::as_array))
                .flatten()
                .filter_map(Value::as_str)
                .filter(|r| *r != "component")
                .map(str::to_string)
                .collect();
            upstream_required.sort();
            let mut ours = spec.required.clone();
            ours.sort();
            assert_eq!(ours, upstream_required, "{name} required props drifted");
        }
    }

    #[test]
    fn excluded_components_are_not_allowed() {
        let c = catalog();
        for name in &c.excluded {
            assert!(
                !c.components.contains_key(name),
                "{name} both allowed and excluded"
            );
        }
        for name in ["Image", "Video", "AudioPlayer"] {
            assert!(c.excluded.iter().any(|e| e == name));
        }
    }
}
