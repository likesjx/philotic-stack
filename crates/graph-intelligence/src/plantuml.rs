use anyhow::Result;

use crate::engine::GraphEngine;
use crate::schema::*;

/// Generate a PlantUML class diagram for all types in a given crate.
pub fn generate_crate_diagram(
    engine: &GraphEngine,
    crate_name: &str,
) -> Result<String> {
    let crate_id = format!("crate:{}", crate_name);
    let mut uml = String::new();
    uml.push_str("@startuml\n");
    uml.push_str(&format!("title {}\n\n", crate_name));

    // Get all modules in this crate
    let edges = engine.get_edges_from(&crate_id)?;
    let module_ids: Vec<String> = edges
        .iter()
        .filter(|e| e.relation == EdgeRelation::Contains)
        .map(|e| e.target_id.clone())
        .collect();

    for module_id in &module_ids {
        let module_node = engine.get_node(module_id)?;
        let _module_name = module_node
            .as_ref()
            .map(|n| n.name.clone())
            .unwrap_or_default();

        // Get types in this module
        let module_edges = engine.get_edges_from(module_id)?;
        for edge in &module_edges {
            if edge.relation != EdgeRelation::Contains {
                continue;
            }

            if let Some(node) = engine.get_node(&edge.target_id)? {
                match node.kind {
                    NodeKind::Type => {
                        let type_kind = node
                            .properties
                            .get("type_kind")
                            .and_then(|v| v.as_str())
                            .unwrap_or("class");

                        // Get the snippet for field/method details
                        let snippets = engine.get_snippets_for_node(&node.id)?;
                        let signature = snippets
                            .first()
                            .map(|s| s.signature.clone())
                            .unwrap_or_default();

                        match type_kind {
                            "struct" => {
                                uml.push_str(&format_struct_uml(&node.name, &signature));
                            }
                            "trait" => {
                                uml.push_str(&format_trait_uml(&node.name, &signature));
                            }
                            "enum" => {
                                uml.push_str(&format_enum_uml(&node.name, &signature));
                            }
                            _ => {
                                uml.push_str(&format!("class {} {{\n}}\n\n", node.name));
                            }
                        }
                    }
                    NodeKind::ImplBlock => {
                        // Draw impl relationships
                        let impl_edges = engine.get_edges_from(&node.id)?;
                        for ie in &impl_edges {
                            if ie.relation == EdgeRelation::TraitImpl {
                                let self_ty = node
                                    .properties
                                    .get("self_type")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("Unknown");
                                let trait_name = ie
                                    .target_id
                                    .split("::")
                                    .last()
                                    .unwrap_or("Unknown");
                                uml.push_str(&format!(
                                    "{} ..|> {} : implements\n",
                                    sanitize_name(self_ty),
                                    sanitize_name(trait_name)
                                ));
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    uml.push_str("@enduml\n");
    Ok(uml)
}

/// Generate a PlantUML diagram for a single module.
pub fn generate_module_diagram(
    engine: &GraphEngine,
    module_id: &str,
) -> Result<String> {
    let module_node = engine.get_node(module_id)?;
    let module_name = module_node
        .as_ref()
        .map(|n| n.name.clone())
        .unwrap_or_else(|| module_id.to_string());

    let mut uml = String::new();
    uml.push_str("@startuml\n");
    uml.push_str(&format!("title Module: {}\n\n", module_name));

    let edges = engine.get_edges_from(module_id)?;
    for edge in &edges {
        if edge.relation != EdgeRelation::Contains {
            continue;
        }
        if let Some(node) = engine.get_node(&edge.target_id)? {
            let snippets = engine.get_snippets_for_node(&node.id)?;
            let sig = snippets
                .first()
                .map(|s| s.signature.clone())
                .unwrap_or_default();

            match node.kind {
                NodeKind::Type => {
                    let type_kind = node
                        .properties
                        .get("type_kind")
                        .and_then(|v| v.as_str())
                        .unwrap_or("class");
                    match type_kind {
                        "struct" => uml.push_str(&format_struct_uml(&node.name, &sig)),
                        "trait" => uml.push_str(&format_trait_uml(&node.name, &sig)),
                        "enum" => uml.push_str(&format_enum_uml(&node.name, &sig)),
                        _ => {
                            uml.push_str(&format!("class {} {{\n}}\n\n", node.name));
                        }
                    }
                }
                NodeKind::Function => {
                    uml.push_str(&format!(
                        "class {} <<function>> {{\n  {}\n}}\n\n",
                        sanitize_name(&node.name),
                        sig.replace('\n', "\n  ")
                    ));
                }
                _ => {}
            }
        }
    }

    // Draw impl/trait relationships
    for edge in &edges {
        if let Some(node) = engine.get_node(&edge.target_id)? {
            if node.kind == NodeKind::ImplBlock {
                let impl_edges = engine.get_edges_from(&node.id)?;
                for ie in &impl_edges {
                    if ie.relation == EdgeRelation::TraitImpl {
                        let self_ty = node
                            .properties
                            .get("self_type")
                            .and_then(|v| v.as_str())
                            .unwrap_or("Unknown");
                        let trait_name = ie
                            .target_id
                            .split("::")
                            .last()
                            .unwrap_or("Unknown");
                        uml.push_str(&format!(
                            "{} ..|> {} : implements\n",
                            sanitize_name(self_ty),
                            sanitize_name(trait_name)
                        ));
                    }
                }
            }
        }
    }

    uml.push_str("@enduml\n");
    Ok(uml)
}

// ── Formatting helpers ──

fn format_struct_uml(name: &str, signature: &str) -> String {
    let mut fields = Vec::new();
    for line in signature.lines() {
        let trimmed = line.trim().trim_end_matches(',');
        if trimmed.contains(':') && !trimmed.starts_with("pub struct") && !trimmed.starts_with("struct") {
            fields.push(format!("  +{}", trimmed));
        }
    }
    let body = if fields.is_empty() {
        String::new()
    } else {
        format!("\n{}\n", fields.join("\n"))
    };
    format!("class {} {{{}}}\n\n", sanitize_name(name), body)
}

fn format_trait_uml(name: &str, signature: &str) -> String {
    let mut methods = Vec::new();
    for line in signature.lines() {
        let trimmed = line.trim().trim_end_matches(';');
        if trimmed.starts_with("fn ") || trimmed.starts_with("async fn ") {
            methods.push(format!("  +{}", trimmed));
        }
    }
    let body = if methods.is_empty() {
        String::new()
    } else {
        format!("\n{}\n", methods.join("\n"))
    };
    format!(
        "interface {} <<trait>> {{{}}}\n\n",
        sanitize_name(name),
        body
    )
}

fn format_enum_uml(name: &str, signature: &str) -> String {
    let mut variants = Vec::new();
    for line in signature.lines() {
        let trimmed = line.trim().trim_end_matches(',');
        if !trimmed.is_empty()
            && !trimmed.starts_with("pub enum")
            && !trimmed.starts_with("enum")
            && !trimmed.starts_with('{')
            && !trimmed.starts_with('}')
        {
            variants.push(format!("  {}", trimmed));
        }
    }
    let body = if variants.is_empty() {
        String::new()
    } else {
        format!("\n{}\n", variants.join("\n"))
    };
    format!("enum {} {{{}}}\n\n", sanitize_name(name), body)
}

fn sanitize_name(name: &str) -> String {
    name.replace('<', "_")
        .replace('>', "_")
        .replace("::", "__")
        .replace(' ', "_")
}
