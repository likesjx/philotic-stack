use anyhow::Result;

use crate::engine::GraphEngine;
use crate::schema::*;

/// Generate a sequence diagram showing function call flow
pub fn generate_sequence_diagram(
    engine: &GraphEngine,
    entry_function: &str, // e.g., "fn:aiua::router::handle_request"
    max_depth: usize,
) -> Result<String> {
    let mut uml = String::new();
    uml.push_str("@startuml\n");
    uml.push_str("!theme plain\n\n");
    uml.push_str(&format!("title Sequence Diagram - {}\n\n", entry_function));

    let mut visited = std::collections::HashSet::new();
    let mut participants = std::collections::HashSet::new();
    let mut calls: Vec<(String, String, String, usize)> = Vec::new(); // (from, to, label, depth)

    // Build call tree
    build_call_tree(
        engine,
        entry_function,
        &mut visited,
        &mut participants,
        &mut calls,
        0,
        max_depth,
    )?;

    // Add participants (unique modules/types)
    let mut participant_list: Vec<_> = participants.into_iter().collect();
    participant_list.sort();
    for p in &participant_list {
        let sanitized = sanitize_seq_id(p);
        uml.push_str(&format!("participant \"{}\" as {}\n", p, sanitized));
    }
    uml.push('\n');

    // Add calls
    for (from, to, label, depth) in calls {
        let from_id = sanitize_seq_id(&from);
        let to_id = sanitize_seq_id(&to);
        let indent = "  ".repeat(depth);
        uml.push_str(&format!("{}{} -> {}: {}\n", indent, from_id, to_id, label));

        // Add activation bars for nested calls
        if depth > 0 {
            uml.push_str(&format!("{}activate {}\n", indent, to_id));
            uml.push_str(&format!("{}{} <-- {}: return\n", indent, from_id, to_id));
            uml.push_str(&format!("{}deactivate {}\n", indent, to_id));
        }
    }

    uml.push_str("\n@enduml\n");
    Ok(uml)
}

fn build_call_tree(
    engine: &GraphEngine,
    function_id: &str,
    visited: &mut std::collections::HashSet<String>,
    participants: &mut std::collections::HashSet<String>,
    calls: &mut Vec<(String, String, String, usize)>,
    depth: usize,
    max_depth: usize,
) -> Result<()> {
    if depth >= max_depth || visited.contains(function_id) {
        return Ok(());
    }
    visited.insert(function_id.to_string());

    if let Some(func) = engine.get_node(function_id)? {
        // Extract module/type as participant
        let participant = extract_participant(&func.name, function_id);
        participants.insert(participant.clone());

        // Get outgoing calls from this function
        let edges = engine.get_edges_from(function_id)?;
        for edge in &edges {
            if edge.relation == EdgeRelation::References {
                if let Some(target) = engine.get_node(&edge.target_id)? {
                    let target_participant = extract_participant(&target.name, &edge.target_id);
                    participants.insert(target_participant.clone());

                    let label = format!(
                        "{}()",
                        target.name.split("::").last().unwrap_or(&target.name)
                    );
                    calls.push((participant.clone(), target_participant, label, depth));

                    // Recurse if it's a function
                    if matches!(target.kind, NodeKind::Function) {
                        build_call_tree(
                            engine,
                            &edge.target_id,
                            visited,
                            participants,
                            calls,
                            depth + 1,
                            max_depth,
                        )?;
                    }
                }
            }
        }
    }

    Ok(())
}

/// Generate state diagram for an enum (showing state machine)
pub fn generate_state_diagram(
    engine: &GraphEngine,
    enum_id: &str, // e.g., "type:aiua::state::AgentState"
) -> Result<String> {
    let mut uml = String::new();
    uml.push_str("@startuml\n");
    uml.push_str("!theme plain\n\n");

    if let Some(enum_node) = engine.get_node(enum_id)? {
        uml.push_str(&format!("title State Diagram - {}\n\n", enum_node.name));

        // Get variants from properties or snippets
        let variants = extract_enum_variants(engine, enum_id, &enum_node)?;

        if variants.is_empty() {
            uml.push_str("note \"No state variants found\" as N1\n");
        } else {
            // Define states
            for variant in &variants {
                let state_id = sanitize_state_id(variant);
                uml.push_str(&format!("state \"{}\" as {}\n", variant, state_id));
            }
            uml.push('\n');

            // Try to find transitions by looking for match statements
            let transitions = find_state_transitions(engine, enum_id, &variants)?;

            if transitions.is_empty() {
                // If no transitions found, show all states with [*] entry
                uml.push_str(&("[*] --> ".to_string() + &sanitize_state_id(&variants[0]) + "\n"));
                for i in 0..variants.len() - 1 {
                    let from = sanitize_state_id(&variants[i]);
                    let to = sanitize_state_id(&variants[i + 1]);
                    uml.push_str(&format!("{} --> {}\n", from, to));
                }
            } else {
                // Show actual transitions
                for (from, to, trigger) in transitions {
                    let from_id = sanitize_state_id(&from);
                    let to_id = sanitize_state_id(&to);
                    uml.push_str(&format!("{} --> {} : {}\n", from_id, to_id, trigger));
                }
            }
        }
    } else {
        uml.push_str(&format!("note \"Enum {} not found\" as N1\n", enum_id));
    }

    uml.push_str("\n@enduml\n");
    Ok(uml)
}

fn extract_enum_variants(
    engine: &GraphEngine,
    enum_id: &str,
    enum_node: &Node,
) -> Result<Vec<String>> {
    let mut variants = Vec::new();

    // Try to get from snippets
    let snippets = engine.get_snippets_for_node(enum_id)?;
    for snippet in &snippets {
        // Parse enum signature like "pub enum AgentState { Idle, Active, Terminated }"
        for line in snippet.signature.lines() {
            let line = line.trim();
            if line.starts_with("pub enum") || line.starts_with("enum") {
                continue;
            }
            if line == "{" || line == "}" {
                continue;
            }
            // Extract variant name (before any data)
            let variant = line
                .trim_start_matches(|c: char| c.is_whitespace() || c == ',')
                .trim_end_matches(|c: char| c.is_whitespace() || c == ',')
                .split(|c: char| c == '(' || c == '{' || c == '<' || c.is_whitespace())
                .next()
                .unwrap_or("")
                .to_string();
            if !variant.is_empty() && variant != "pub" {
                variants.push(variant);
            }
        }
    }

    // Also try properties
    if variants.is_empty() {
        if let Some(v) = enum_node.properties.get("variants") {
            if let Some(arr) = v.as_array() {
                for val in arr {
                    if let Some(s) = val.as_str() {
                        variants.push(s.to_string());
                    }
                }
            }
        }
    }

    Ok(variants)
}

fn find_state_transitions(
    engine: &GraphEngine,
    enum_id: &str,
    variants: &[String],
) -> Result<Vec<(String, String, String)>> {
    let mut transitions = Vec::new();

    // Find functions that match on this enum
    let edges = engine.get_edges_to(enum_id)?;
    for edge in &edges {
        if edge.relation == EdgeRelation::References {
            if let Some(func) = engine.get_node(&edge.source_id)? {
                if matches!(func.kind, NodeKind::Function) {
                    // Look for this function's body to find state transitions
                    let func_snippets = engine.get_snippets_for_node(&func.id)?;
                    for snippet in &func_snippets {
                        // Parse for match arms like "AgentState::Idle => AgentState::Active"
                        let body = snippet.body.as_deref().unwrap_or("");
                        for (i, line) in body.lines().enumerate() {
                            for variant in variants {
                                if line.contains(&format!("::{} =>", variant))
                                    || line.contains(&format!("{} =>", variant))
                                {
                                    // Try to find the target state in the same or next line
                                    let search_text = if i + 1 < body.lines().count() {
                                        format!(
                                            "{} {}",
                                            line,
                                            body.lines().nth(i + 1).unwrap_or("")
                                        )
                                    } else {
                                        line.to_string()
                                    };

                                    for target in variants {
                                        if target != variant
                                            && search_text.contains(&format!("::{} ", target))
                                            || search_text.contains(&format!("{}(", target))
                                        {
                                            let trigger =
                                                func.name.split("::").last().unwrap_or(&func.name);
                                            transitions.push((
                                                variant.clone(),
                                                target.clone(),
                                                trigger.to_string(),
                                            ));
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // If we found transitions, add entry point
    if !transitions.is_empty() && !variants.is_empty() {
        transitions.push(("[*]".to_string(), variants[0].clone(), "init".to_string()));
    }

    Ok(transitions)
}

/// Generate a module interaction diagram showing who calls whom
pub fn generate_module_interaction(engine: &GraphEngine, crate_name: &str) -> Result<String> {
    let mut uml = String::new();
    uml.push_str("@startuml\n");
    uml.push_str("!theme plain\n\n");
    uml.push_str(&format!("title Module Interactions - {}\n\n", crate_name));

    let crate_id = format!("crate:{}", crate_name);
    let mut modules = Vec::new();
    let mut interactions: Vec<(String, String, usize)> = Vec::new(); // (from, to, count)

    // Get all modules in crate
    let edges = engine.get_edges_from(&crate_id)?;
    for edge in &edges {
        if edge.relation == EdgeRelation::Contains {
            if let Some(module) = engine.get_node(&edge.target_id)? {
                if matches!(module.kind, NodeKind::Module) {
                    modules.push(module.name.clone());

                    // Count imports from this module
                    let mod_edges = engine.get_edges_from(&module.id)?;
                    for me in &mod_edges {
                        if me.relation == EdgeRelation::Imports {
                            let target_mod =
                                me.target_id.split("::").last().unwrap_or(&me.target_id);
                            let source_mod = module.name.split("::").last().unwrap_or(&module.name);
                            if source_mod != target_mod {
                                interactions.push((
                                    source_mod.to_string(),
                                    target_mod.to_string(),
                                    1,
                                ));
                            }
                        }
                    }
                }
            }
        }
    }

    // Define participants
    for m in &modules {
        let short = m.split("::").last().unwrap_or(m);
        uml.push_str(&format!(
            "participant \"{}\" as {}\n",
            short,
            sanitize_seq_id(short)
        ));
    }
    uml.push('\n');

    // Show interactions
    for (from, to, count) in interactions {
        let from_id = sanitize_seq_id(&from);
        let to_id = sanitize_seq_id(&to);
        uml.push_str(&format!("{} -> {} : {} refs\n", from_id, to_id, count));
    }

    uml.push_str("\n@enduml\n");
    Ok(uml)
}

// ── Helpers ──

fn sanitize_seq_id(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' => c,
            '_' => '_',
            ':' | '-' | '.' | '<' | '>' | '(' | ')' | '[' | ']' | ' ' => '_',
            _ => '_',
        })
        .collect::<String>()
}

fn sanitize_state_id(name: &str) -> String {
    // For state diagrams, keep it simple
    name.replace(|c: char| !c.is_alphanumeric() && c != '_', "_")
        .to_lowercase()
}

fn extract_participant(name: &str, _node_id: &str) -> String {
    // Extract module or type name from full path
    let parts: Vec<&str> = name.split("::").collect();
    if parts.len() >= 2 {
        // Return crate::module or just module
        format!("{}::{}", parts[0], parts[1])
    } else {
        name.to_string()
    }
}
