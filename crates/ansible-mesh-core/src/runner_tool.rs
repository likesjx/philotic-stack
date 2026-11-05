//! Runner-owned descriptions are capabilities, never grants. Hotels project
//! them only through an independently authorized endpoint or session binding.
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunnerToolDescriptor {
    pub name: String,
    pub version: u32,
    pub description: String,
    pub input_schema: Value,
    pub read_only: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunnerToolAdvertisement {
    pub protocol_version: u32,
    /// Monotonic within a connection. A reconnect starts a new incarnation.
    pub revision: u64,
    pub tools: Vec<RunnerToolDescriptor>,
}

impl RunnerToolDescriptor {
    fn supported_schema(&self) -> bool {
        let Some(root) = self.input_schema.as_object() else {
            return false;
        };
        let Some(properties) = self.input_schema["properties"].as_object() else {
            return false;
        };
        root.keys().all(|key| {
            ["type", "properties", "required", "additionalProperties"].contains(&key.as_str())
        }) && self.input_schema["type"] == "object"
            && self.input_schema["additionalProperties"] == false
            && properties.len() <= 16
            && self.input_schema["required"]
                .as_array()
                .is_some_and(|required| {
                    required.iter().all(|name| {
                        name.as_str()
                            .is_some_and(|name| properties.contains_key(name))
                    })
                })
            && properties.values().all(|schema| {
                let Some(fields) = schema.as_object() else {
                    return false;
                };
                let allowed = match schema["type"].as_str() {
                    Some("string") => {
                        &["type", "description", "default", "minLength", "maxLength"][..]
                    }
                    Some("integer") => {
                        &["type", "description", "default", "minimum", "maximum"][..]
                    }
                    _ => return false,
                };
                fields.keys().all(|key| allowed.contains(&key.as_str()))
                    && ["minLength", "maxLength"].iter().all(|key| {
                        fields
                            .get(*key)
                            .is_none_or(|value| value.as_u64().is_some())
                    })
                    && ["minimum", "maximum"].iter().all(|key| {
                        fields
                            .get(*key)
                            .is_none_or(|value| value.as_i64().is_some())
                    })
            })
    }
    /// The initial wire contract supports flat, bounded read arguments only.
    /// Fail closed on unsupported schemas instead of pretending to validate
    /// general JSON Schema. Richer contracts need a versioned extension.
    pub fn validate_arguments(&self, arguments: &Value) -> Result<(), &'static str> {
        if !self.supported_schema() {
            return Err("unsupported argument schema");
        }
        let object = arguments.as_object().ok_or("arguments must be an object")?;
        let properties = self.input_schema["properties"]
            .as_object()
            .ok_or("missing properties")?;
        if let Some(required) = self.input_schema["required"].as_array() {
            for name in required {
                if !name.as_str().is_some_and(|name| object.contains_key(name)) {
                    return Err("missing required argument");
                }
            }
        }
        for (name, value) in object {
            let schema = properties.get(name).ok_or("unknown argument")?;
            match schema["type"].as_str() {
                Some("string") => {
                    let value = value.as_str().ok_or("expected string")?;
                    let length = value.chars().count() as u64;
                    if schema["minLength"].as_u64().is_some_and(|min| length < min)
                        || schema["maxLength"].as_u64().is_some_and(|max| length > max)
                        || value.trim().is_empty()
                    {
                        return Err("string argument out of bounds");
                    }
                }
                Some("integer") => {
                    let value = value.as_i64().ok_or("expected integer")?;
                    if schema["minimum"].as_i64().is_some_and(|min| value < min)
                        || schema["maximum"].as_i64().is_some_and(|max| value > max)
                    {
                        return Err("integer argument out of bounds");
                    }
                }
                _ => return Err("unsupported argument schema"),
            }
        }
        Ok(())
    }
}

impl RunnerToolAdvertisement {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.protocol_version != 1 || self.revision == 0 || self.tools.len() > 32 {
            return Err("unsupported advertisement version or size");
        }
        let mut names = std::collections::HashSet::new();
        for tool in &self.tools {
            if tool.version == 0
                || tool.name.is_empty()
                || tool.name.len() > 128
                || !tool
                    .name
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
                || !names.insert(&tool.name)
                || tool.description.trim().is_empty()
                || tool.description.len() > 4096
                || !tool.supported_schema()
                || tool.input_schema.to_string().len() > 32768
            {
                return Err("invalid or duplicate runner tool descriptor");
            }
        }
        Ok(())
    }
}
