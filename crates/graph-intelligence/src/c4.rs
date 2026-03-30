use anyhow::Result;

use crate::engine::GraphEngine;
use crate::schema::*;

/// Generate C4 Context diagram (C1) - System level
pub fn generate_c4_context(
    engine: &GraphEngine,
    system_name: &str,
) -> Result<String> {
    let mut uml = String::new();
    uml.push_str("@startuml\n");
    uml.push_str("!include https://raw.githubusercontent.com/plantuml-stdlib/C4-PlantUML/master/C4_Context.puml\n\n");
    uml.push_str(&format!("LAYOUT_WITH_LEGEND()\n\n"));
    uml.push_str(&format!("title C4 Context Diagram - {}\n\n", system_name));

    // Find the system
    let system_id = format!("system:{}", system_name);
    let edges = engine.get_edges_from(&system_id)?;

    // Add system
    uml.push_str(&format!(
        "System({}, \"{}\", \"Philotic Stack Agent OS\")\n\n",
        sanitize_c4_id(system_name),
        system_name
    ));

    // Find external systems (actors, external dependencies)
    for edge in &edges {
        if edge.relation == EdgeRelation::DependsOn || edge.relation == EdgeRelation::References {
            if let Some(node) = engine.get_node(&edge.target_id)? {
                if matches!(node.kind, NodeKind::Domain | NodeKind::Skill) {
                    let ext_id = sanitize_c4_id(&node.name);
                    uml.push_str(&format!(
                        "System_Ext({}, \"{}\", \"{}\")\n",
                        ext_id,
                        node.name,
                        get_description(&node.properties, &node.kind)
                    ));
                    uml.push_str(&format!(
                        "Rel({}, {}, \"{}\")\n",
                        sanitize_c4_id(system_name),
                        ext_id,
                        edge.relation.as_str()
                    ));
                }
            }
        }
    }

    // Add actors/agents
    for edge in &edges {
        if edge.relation == EdgeRelation::Governs {
            if let Some(node) = engine.get_node(&edge.target_id)? {
                if matches!(node.kind, NodeKind::Agent) {
                    let agent_id = sanitize_c4_id(&node.name);
                    uml.push_str(&format!(
                        "Person({}, \"{}\", \"AI Agent\")\n",
                        agent_id,
                        node.name
                    ));
                    uml.push_str(&format!(
                        "Rel({}, {}, \"uses\")\n",
                        agent_id,
                        sanitize_c4_id(system_name)
                    ));
                }
            }
        }
    }

    uml.push_str("\n@enduml\n");
    Ok(uml)
}

/// Generate C4 Container diagram (C2) - Crate/process level
pub fn generate_c4_container(
    engine: &GraphEngine,
    system_name: &str,
) -> Result<String> {
    let mut uml = String::new();
    uml.push_str("@startuml\n");
    uml.push_str("!include https://raw.githubusercontent.com/plantuml-stdlib/C4-PlantUML/master/C4_Container.puml\n\n");
    uml.push_str("LAYOUT_WITH_LEGEND()\n\n");
    uml.push_str(&format!("title C4 Container Diagram - {}\n\n", system_name));

    // Get all crates
    let crates = engine.query_nodes(Some(NodeKind::Crate), None)?;

    // System boundary
    let system_id = sanitize_c4_id(system_name);
    uml.push_str(&format!(
        "System_Boundary({}, \"{}\") {{\n",
        system_id,
        system_name
    ));

    // Add containers (crates)
    for crate_node in &crates {
        let container_id = sanitize_c4_id(&crate_node.name);
        let tech = get_crate_tech(crate_node);
        let desc = get_description(&crate_node.properties, &NodeKind::Crate);

        uml.push_str(&format!(
            "    Container({}, \"{}\", \"{}\", \"{}\")\n",
            container_id,
            crate_node.name,
            tech,
            desc
        ));
    }

    uml.push_str("}\n\n");

    // Add relationships between containers
    for crate_node in &crates {
        let edges = engine.get_edges_from(&crate_node.id)?;
        for edge in &edges {
            if edge.relation == EdgeRelation::DependsOn {
                let source_id = sanitize_c4_id(&crate_node.name);
                let target_name = edge.target_id.replace("crate:", "");
                let target_id = sanitize_c4_id(&target_name);
                uml.push_str(&format!(
                    "Rel({}, {}, \"uses\")\n",
                    source_id,
                    target_id
                ));
            }
        }
    }

    uml.push_str("\n@enduml\n");
    Ok(uml)
}

/// Generate C4 Component diagram (C3) - Module level for a crate
pub fn generate_c4_component(
    engine: &GraphEngine,
    crate_name: &str,
) -> Result<String> {
    let mut uml = String::new();
    uml.push_str("@startuml\n");
    uml.push_str("!include https://raw.githubusercontent.com/plantuml-stdlib/C4-PlantUML/master/C4_Component.puml\n\n");
    uml.push_str("LAYOUT_WITH_LEGEND()\n\n");
    uml.push_str(&format!("title C4 Component Diagram - {}\n\n", crate_name));

    let crate_id = format!("crate:{}", crate_name);

    // Container boundary
    let container_id = sanitize_c4_id(crate_name);
    uml.push_str(&format!(
        "Container_Boundary({}, \"{}\") {{\n",
        container_id,
        crate_name
    ));

    // Get modules in this crate
    let edges = engine.get_edges_from(&crate_id)?;
    for edge in &edges {
        if edge.relation == EdgeRelation::Contains {
            if let Some(module) = engine.get_node(&edge.target_id)? {
                if matches!(module.kind, NodeKind::Module) {
                    let comp_id = sanitize_c4_id(&module.name);
                    let desc = get_description(&module.properties, &NodeKind::Module);
                    let tech = "Rust Module";

                    uml.push_str(&format!(
                        "    Component({}, \"{}\", \"{}\", \"{}\")\n",
                        comp_id,
                        module.name,
                        tech,
                        desc
                    ));
                }
            }
        }
    }

    uml.push_str("}\n\n");

    // Add relationships between modules (via imports/dependencies)
    for edge in &edges {
        if edge.relation == EdgeRelation::Contains {
            if let Some(module) = engine.get_node(&edge.target_id)? {
                if matches!(module.kind, NodeKind::Module) {
                    let mod_edges = engine.get_edges_from(&module.id)?;
                    for mod_edge in &mod_edges {
                        if mod_edge.relation == EdgeRelation::Imports {
                            let source_id = sanitize_c4_id(&module.name);
                            let target_parts: Vec<&str> = mod_edge.target_id.split("::").collect();
                            if let Some(target_name) = target_parts.last() {
                                let target_id = sanitize_c4_id(target_name);
                                uml.push_str(&format!(
                                    "Rel({}, {}, \"imports\")\n",
                                    source_id,
                                    target_id
                                ));
                            }
                        }
                    }
                }
            }
        }
    }

    uml.push_str("\n@enduml\n");
    Ok(uml)
}

/// Generate proposal architecture diagram (C4-like process view)
pub fn generate_proposal_architecture(
    engine: &GraphEngine,
    proposal_id: &str,
) -> Result<String> {
    let mut uml = String::new();
    uml.push_str("@startuml\n");
    uml.push_str("!theme plain\n\n");
    uml.push_str(&format!("title Proposal Architecture - {}\n\n", proposal_id));

    // Get proposal
    let prop_id = if proposal_id.starts_with("proposal:") {
        proposal_id.to_string()
    } else {
        format!("proposal:{}", proposal_id)
    };

    if let Some(prop) = engine.get_node(&prop_id)? {
        // Proposal box
        uml.push_str(&format!(
            "rectangle \"{}\" as {} #LightBlue {{\n",
            prop.name,
            sanitize_c4_id(&prop.name)
        ));

        // Extract status and seams from properties
        let status = prop.properties
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("proposed");

        uml.push_str(&format!("  status: {}\n", status));

        // Active seams
        if let Some(seams) = prop.properties.get("active_seams") {
            if let Some(seam_array) = seams.as_array() {
                if !seam_array.is_empty() {
                    uml.push_str("  seams:\\n");
                    for seam in seam_array {
                        if let Some(s) = seam.as_str() {
                            uml.push_str(&format!("    - {}\\n", s));
                        }
                    }
                }
            }
        }

        uml.push_str("}\n\n");

        // Find seams this proposal implements
        let edges = engine.get_edges_from(&prop_id)?;
        for edge in &edges {
            if edge.relation == EdgeRelation::Implements {
                if let Some(seam) = engine.get_node(&edge.target_id)? {
                    let seam_id = sanitize_c4_id(&seam.name);
                    uml.push_str(&format!(
                        "rectangle \"{}\" as {} #LightYellow\n",
                        seam.name,
                        seam_id
                    ));
                    uml.push_str(&format!(
                        "{} --> {} : implements\n",
                        sanitize_c4_id(&prop.name),
                        seam_id
                    ));

                    // Find code that implements this seam
                    let seam_edges = engine.get_edges_from(&seam.id)?;
                    for se in &seam_edges {
                        if se.relation == EdgeRelation::ImplementedBy {
                            if let Some(code) = engine.get_node(&se.target_id)? {
                                let code_id = sanitize_c4_id(&code.name);
                                let color = match code.kind {
                                    NodeKind::Module => "#LightGreen",
                                    NodeKind::Type => "#Pink",
                                    NodeKind::Function => "#Lavender",
                                    _ => "#White",
                                };
                                uml.push_str(&format!(
                                    "rectangle \"{} [{}]\" as {} {}\n",
                                    code.name,
                                    code.kind.as_str(),
                                    code_id,
                                    color
                                ));
                                uml.push_str(&format!(
                                    "{} --> {} : implements\n",
                                    seam_id,
                                    code_id
                                ));
                            }
                        }
                    }
                }
            }
        }
    }

    uml.push_str("\n@enduml\n");
    Ok(uml)
}

/// Generate seam implementation diagram showing slices
pub fn generate_seam_detail(
    engine: &GraphEngine,
    seam_id: &str,
) -> Result<String> {
    let mut uml = String::new();
    uml.push_str("@startuml\n");
    uml.push_str("!theme plain\n\n");

    let seam_id_full = if seam_id.starts_with("seam:") {
        seam_id.to_string()
    } else {
        format!("seam:{}", seam_id)
    };

    if let Some(seam) = engine.get_node(&seam_id_full)? {
        uml.push_str(&format!(
            "title Seam Detail - {}\n\n",
            seam.name
        ));

        // Seam at top
        let seam_sid = sanitize_c4_id(&seam.name);
        uml.push_str(&format!(
            "rectangle \"{}\" as {} #Orange {{\n",
            seam.name,
            seam_sid
        ));

        // Get description from properties
        let desc = seam.properties
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if !desc.is_empty() {
            uml.push_str(&format!("  {}\n", desc.replace('\n', "\\n")));
        }
        uml.push_str("}\n\n");

        // Find parent proposal
        let incoming = engine.get_edges_to(&seam_id_full)?;
        for edge in &incoming {
            if edge.relation == EdgeRelation::Implements {
                if let Some(prop) = engine.get_node(&edge.source_id)? {
                    let prop_sid = sanitize_c4_id(&prop.name);
                    uml.push_str(&format!(
                        "rectangle \"{} [proposal]\" as {} #LightBlue\n",
                        prop.name,
                        prop_sid
                    ));
                    uml.push_str(&format!(
                        "{} --> {} : governs\n",
                        prop_sid,
                        seam_sid
                    ));
                }
            }
        }

        // Find implementing code
        let outgoing = engine.get_edges_from(&seam_id_full)?;
        for edge in &outgoing {
            if edge.relation == EdgeRelation::ImplementedBy {
                if let Some(code) = engine.get_node(&edge.target_id)? {
                    let code_sid = sanitize_c4_id(&code.name);
                    let color = match code.kind {
                        NodeKind::Module => "#LightGreen",
                        NodeKind::Type => "#Pink",
                        NodeKind::Function => "#Lavender",
                        _ => "#White",
                    };
                    let label = match code.kind {
                        NodeKind::Module => "implemented in",
                        NodeKind::Type => "defined as",
                        NodeKind::Function => "implemented by",
                        _ => "related to",
                    };
                    uml.push_str(&format!(
                        "rectangle \"{}\" as {} {}\n",
                        code.name,
                        code_sid,
                        color
                    ));
                    uml.push_str(&format!(
                        "{} --> {} : {}\n",
                        seam_sid,
                        code_sid,
                        label
                    ));
                }
            }
        }
    }

    uml.push_str("\n@enduml\n");
    Ok(uml)
}

// ── Helpers ──

fn sanitize_c4_id(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '_' => c,
            '-' | ':' | '.' | '/' => '_',
            _ => '_',
        })
        .collect::<String>()
        .replace("crate_", "")
        .replace("module_", "")
        .replace("type_", "")
        .replace("proposal_", "")
        .replace("seam_", "")
}

fn get_description(props: &serde_json::Value, kind: &NodeKind) -> String {
    props.get("description")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("{:?}", kind))
}

fn get_crate_tech(crate_node: &Node) -> String {
    // Determine tech from crate properties or name patterns
    if crate_node.name.contains("web") || crate_node.name.contains("http") {
        "Rust / HTTP / Axum".to_string()
    } else if crate_node.name.contains("mcp") || crate_node.name.contains("protocol") {
        "Rust / Protocol / MCP".to_string()
    } else if crate_node.name.contains("graph") || crate_node.name.contains("intelligence") {
        "Rust / Graph / SQLite".to_string()
    } else {
        "Rust".to_string()
    }
}
