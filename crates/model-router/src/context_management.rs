//! Deterministic request-local context selection. No persistence or model calls.
//! Until an exact tokenizer is available for the resolved model, UTF-8 bytes
//! plus framing overhead are a conservative token estimate, not provider usage.
use crate::controller::{ControllerTask, TaskKind};
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ContextLimits {
    pub input_tokens: usize,
    pub output_tokens: usize,
    pub history_tokens: usize,
    pub memory_tokens: usize,
    pub tool_result_tokens: usize,
    pub tool_schema_tokens: usize,
    pub user_tokens: usize,
    pub mandatory_tokens: usize,
}
impl Default for ContextLimits {
    fn default() -> Self {
        Self {
            input_tokens: 32_768,
            output_tokens: 4_096,
            history_tokens: 8_192,
            memory_tokens: 4_096,
            tool_result_tokens: 8_192,
            tool_schema_tokens: 8_192,
            user_tokens: 16_384,
            mandatory_tokens: 16_384,
        }
    }
}
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ContextAccounting {
    pub estimator: &'static str,
    pub model_context_limit: usize,
    pub reported_context_models: usize,
    pub unknown_context_models: usize,
    pub unknown_output_models: usize,
    pub input_limit: usize,
    pub output_limit: usize,
    pub history_selected: usize,
    pub history_dropped: usize,
    pub memory_selected: usize,
    pub memory_dropped: usize,
    pub tool_pairs_selected: usize,
    pub tool_pairs_dropped: usize,
    pub schema_estimate: usize,
    pub user_estimate: usize,
    pub mandatory_estimate: usize,
    pub history_estimate: usize,
    pub memory_estimate: usize,
    pub tool_result_estimate: usize,
    pub rendered_estimate: usize,
}
pub const UNKNOWN_CONTEXT_CEILING: usize = 16_384;
pub const UNKNOWN_OUTPUT_CEILING: usize = 4_096;

/// Runtime-resolved reports, not capability authority. Generic SetConfig has no
/// authenticated catalog owner, so reports can only tighten conservative limits.
/// No attestation or caller-controlled expansion bypass is implemented here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelCapabilityRecord {
    pub provider: String,
    pub model_id: String,
    pub context_tokens: Option<usize>,
    pub output_tokens: Option<usize>,
    pub source: String,
}

/// Existing compact hotel catalog reports ctx; `out` is price, NOT output size.
/// Missing/invalid records and ambiguous duplicate IDs remain unknown.
pub fn parse_openrouter_capabilities(raw: &str) -> Vec<ModelCapabilityRecord> {
    if raw.len() > 2_000_000 {
        return Vec::new();
    }
    let Ok(rows) = serde_json::from_str::<Vec<Value>>(raw) else {
        return Vec::new();
    };
    let mut ids = std::collections::BTreeMap::<String, usize>::new();
    for row in &rows {
        if let Some(id) = row.get("id").and_then(Value::as_str) {
            *ids.entry(id.to_owned()).or_default() += 1;
        }
    }
    rows.into_iter()
        .filter_map(|row| {
            let id = row.get("id")?.as_str()?;
            if id.trim().is_empty() || ids.get(id) != Some(&1) {
                return None;
            }
            let ctx = row
                .get("ctx")
                .and_then(Value::as_u64)
                .and_then(|n| u32::try_from(n).ok())
                .map(|n| n as usize)
                .filter(|n| *n > 0);
            Some(ModelCapabilityRecord {
                provider: "openrouter".into(),
                model_id: id.into(),
                context_tokens: ctx,
                output_tokens: None,
                source: "hotel_config:model_catalog.openrouter (unversioned snapshot)".into(),
            })
        })
        .collect()
}

pub fn resolve_capabilities(
    provider: &str,
    models: Vec<String>,
    catalog: &[ModelCapabilityRecord],
) -> Vec<ModelCapabilityRecord> {
    let mut unique = std::collections::BTreeSet::new();
    models
        .into_iter()
        .filter(|id| unique.insert(id.clone()))
        .map(|id| {
            let mut matches = catalog
                .iter()
                .filter(|c| c.provider == provider && c.model_id == id);
            let first = matches.next();
            let unambiguous = if matches.next().is_none() {
                first.cloned()
            } else {
                None
            };
            unambiguous.unwrap_or(ModelCapabilityRecord {
                provider: provider.into(),
                model_id: id,
                context_tokens: None,
                output_tokens: None,
                source: "unknown_conservative_ceiling".into(),
            })
        })
        .collect()
}

fn model_bounds(task: &ControllerTask) -> (usize, usize) {
    let mut bounds = task.resolved_context_capabilities.iter().map(|c| {
        (
            c.context_tokens
                .filter(|n| *n > 0)
                .unwrap_or(UNKNOWN_CONTEXT_CEILING)
                .min(UNKNOWN_CONTEXT_CEILING),
            c.output_tokens
                .filter(|n| *n > 0)
                .unwrap_or(UNKNOWN_OUTPUT_CEILING)
                .min(UNKNOWN_OUTPUT_CEILING),
        )
    });
    let Some(first) = bounds.next() else {
        return (UNKNOWN_CONTEXT_CEILING, UNKNOWN_OUTPUT_CEILING);
    };
    bounds.fold(first, |a, b| (a.0.min(b.0), a.1.min(b.1)))
}

pub fn estimate_json(value: &Value) -> Result<usize> {
    Ok(serde_json::to_vec(value)?.len().saturating_add(256))
}
fn positive(value: &Value) -> Result<usize> {
    let n = value
        .as_u64()
        .or_else(|| value.as_str().and_then(|s| s.parse().ok()));
    match n.and_then(|n| usize::try_from(n).ok()).filter(|n| *n > 0) {
        Some(n) => Ok(n),
        None => bail!("context_budget_invalid: output limit must be a positive integer"),
    }
}
pub fn limits(task: &ControllerTask) -> Result<(ContextLimits, usize)> {
    let mut l: ContextLimits = match task.provider_options.get("context_limits") {
        Some(v) => serde_json::from_value(v.clone())?,
        None => ContextLimits::default(),
    };
    let a = task
        .provider_options
        .get("max_tokens")
        .map(positive)
        .transpose()?;
    let b = task
        .provider_options
        .get("max_completion_tokens")
        .map(positive)
        .transpose()?;
    if a.is_some() && b.is_some() && a != b {
        bail!("context_budget_invalid: conflicting output limits");
    }
    if let Some(n) = a.or(b) {
        l.output_tokens = n;
    }
    let (hard, output_ceiling) = model_bounds(task);
    if l.output_tokens == 0
        || l.output_tokens >= hard
        || l.input_tokens == 0
        || l.output_tokens > output_ceiling
    {
        bail!("context_budget_invalid: input/output limits exceed model context");
    }
    // An explicit long-task override must itself fit the hard model ceiling.
    if task
        .provider_options
        .get("context_limits")
        .and_then(|v| v.get("input_tokens"))
        .is_some()
        && l.input_tokens > hard - l.output_tokens
    {
        bail!("context_budget_invalid: long-task override exceeds model context");
    }
    l.input_tokens = l.input_tokens.min(hard - l.output_tokens);
    Ok((l, hard))
}
fn projection_cost(p: &crate::controller::ProjectionItem) -> usize {
    p.text
        .clone()
        .map(|t| t.len())
        .unwrap_or(0)
        .saturating_add(64)
}
/// Returns a clone: retries, checkpoints, durable messages, and memories are
/// never rewritten by selection. Tool schemas are mandatory in this slice;
/// dropping one without knowing plan/tool authority would break execution.
pub fn prepare(task: &ControllerTask) -> Result<(ControllerTask, ContextAccounting)> {
    let (l, hard) = limits(task)?;
    let mut t = task.clone();
    if !matches!(task.kind, TaskKind::TextGenerate) {
        bail!("context_budget_invalid: non-text task");
    }
    let user = t
        .context
        .active_turn
        .as_ref()
        .and_then(|v| v.text_content())
        .or_else(|| t.prompt.clone())
        .unwrap_or_default();
    if user.len() > l.user_tokens {
        bail!("context_budget_exceeded: current user request cannot fit user category");
    }
    let schema = estimate_json(&Value::Array(t.tools.clone()))?;
    if schema > l.tool_schema_tokens {
        bail!("context_budget_exceeded: mandatory tool schemas cannot fit");
    }
    // Recall duplicated into the legacy memory lane is rendered once.
    let recalled = t.context.recalled_memory.clone();
    t.context
        .memory
        .retain(|m| !recalled.iter().any(|r| r.text == m.text));
    let mandatory_estimate = t
        .context
        .identity
        .iter()
        .chain(&t.context.instructions)
        .map(projection_cost)
        .sum::<usize>()
        + t.context
            .active_plan
            .as_ref()
            .map(|v| v.to_string().len())
            .unwrap_or(0);
    if mandatory_estimate > l.mandatory_tokens {
        bail!("context_budget_exceeded: required instructions cannot fit mandatory category");
    }
    let original_memory = t.context.memory.len() + t.context.recalled_memory.len();
    let mut seen = std::collections::BTreeSet::new();
    let mut memory_cost = 0usize;
    for lane in [&mut t.context.memory, &mut t.context.recalled_memory] {
        lane.retain(|p| {
            let text = p.text.clone().unwrap_or_default();
            let key = text.split_whitespace().collect::<Vec<_>>().join(" ");
            let cost = projection_cost(p);
            if !seen.insert(key) || cost > l.memory_tokens.saturating_sub(memory_cost) {
                return false;
            }
            memory_cost += cost;
            true
        });
    }
    let original_history = t.context.dialogue_window.len();
    // Select coherent adjacent user/assistant pairs, newest first. A singleton
    // is retained as one item; never cut a message or split a complete pair.
    let mut groups = Vec::new();
    let mut i = 0;
    while i < t.context.dialogue_window.len() {
        let n = if t.context.dialogue_window[i].role.as_deref() == Some("user")
            && t.context
                .dialogue_window
                .get(i + 1)
                .and_then(|v| v.role.as_deref())
                == Some("assistant")
        {
            2
        } else {
            1
        };
        groups.push(t.context.dialogue_window[i..i + n].to_vec());
        i += n;
    }
    let mut selected = Vec::new();
    let mut used = 0usize;
    for g in groups.into_iter().rev() {
        let cost = g
            .iter()
            .map(|v| v.text_content().unwrap_or_default().len() + 64)
            .sum::<usize>();
        if cost <= l.history_tokens.saturating_sub(used) {
            used += cost;
            selected.push(g);
        }
    }
    t.context.dialogue_window = selected.into_iter().rev().flatten().collect();
    let original_tools = t.context.tool_history.len();
    if let Some(last) = t.context.tool_history.last() {
        let cost = last.result.len() + serde_json::to_vec(&last.arguments)?.len() + 128;
        if cost > l.tool_result_tokens {
            bail!("context_budget_exceeded: latest required tool result cannot fit");
        }
    }

    let mut used = 0usize;
    t.context.tool_history = t
        .context
        .tool_history
        .into_iter()
        .rev()
        .filter(|p| {
            let cost = p.result.len()
                + serde_json::to_vec(&p.arguments)
                    .map(|v| v.len())
                    .unwrap_or(usize::MAX / 2)
                + 128;
            if cost > l.tool_result_tokens.saturating_sub(used) {
                false
            } else {
                used += cost;
                true
            }
        })
        .collect();
    t.context.tool_history.reverse();
    // The renderer consumes structured context when available, never the flat
    // legacy envelope. Bare requests continue to use their original prompt.
    let cost = |x: &ControllerTask| {
        serde_json::to_vec(&x.composed_prompt_text().unwrap_or_default())
            .map(|v| v.len())
            .unwrap_or(usize::MAX / 2)
            .saturating_add(schema)
            .saturating_add(512)
    };
    while cost(&t) > l.input_tokens {
        if !t.context.dialogue_window.is_empty() {
            let pair = t.context.dialogue_window[0].role.as_deref() == Some("user")
                && t.context
                    .dialogue_window
                    .get(1)
                    .and_then(|v| v.role.as_deref())
                    == Some("assistant");
            t.context.dialogue_window.drain(..if pair { 2 } else { 1 });
        } else if t.context.tool_history.len() > 1 {
            t.context.tool_history.remove(0);
        } else if !t.context.memory.is_empty() {
            t.context.memory.pop();
        } else if !t.context.recalled_memory.is_empty() {
            t.context.recalled_memory.pop();
        } else {
            bail!(
                "context_budget_exceeded: mandatory user/instructions/schemas cannot fit model input"
            );
        }
    }
    t.provider_options
        .insert("max_tokens".into(), Value::from(l.output_tokens));
    t.provider_options.remove("max_completion_tokens");
    let selected_memory = t.context.memory.len() + t.context.recalled_memory.len();
    let accounting = ContextAccounting {
        estimator: "conservative_utf8_bytes_plus_framing",
        model_context_limit: hard,
        reported_context_models: t
            .resolved_context_capabilities
            .iter()
            .filter(|c| c.context_tokens.is_some_and(|n| n > 0))
            .count(),
        unknown_context_models: t
            .resolved_context_capabilities
            .iter()
            .filter(|c| c.context_tokens.is_none_or(|n| n == 0))
            .count()
            .max(usize::from(t.resolved_context_capabilities.is_empty())),
        unknown_output_models: t
            .resolved_context_capabilities
            .iter()
            .filter(|c| c.output_tokens.is_none_or(|n| n == 0))
            .count()
            .max(usize::from(t.resolved_context_capabilities.is_empty())),
        input_limit: l.input_tokens,
        output_limit: l.output_tokens,
        history_selected: t.context.dialogue_window.len(),
        history_dropped: original_history - t.context.dialogue_window.len(),
        memory_selected: selected_memory,
        memory_dropped: original_memory - selected_memory,
        tool_pairs_selected: t.context.tool_history.len(),
        tool_pairs_dropped: original_tools - t.context.tool_history.len(),
        schema_estimate: schema,
        user_estimate: user.len(),
        mandatory_estimate,
        history_estimate: t
            .context
            .dialogue_window
            .iter()
            .map(|v| v.text_content().unwrap_or_default().len() + 64)
            .sum(),
        memory_estimate: t
            .context
            .memory
            .iter()
            .chain(&t.context.recalled_memory)
            .map(projection_cost)
            .sum(),
        tool_result_estimate: t
            .context
            .tool_history
            .iter()
            .map(|v| v.result.len() + v.arguments.to_string().len() + 128)
            .sum(),
        rendered_estimate: cost(&t),
    };
    Ok((t, accounting))
}
/// The final provider wire envelope is checked after serialization, including
/// wrappers, native tool schemas and instructions. Fail rather than send an
/// over-budget request; actual billed usage remains separately measured.
pub fn account_wire(body: &Value, task: &ControllerTask) -> Result<()> {
    let (l, mut hard) = limits(task)?;
    // A provider's configured default or OpenRouter fallback can differ from
    // task.model. Every advertised candidate must fit, not just the pin.
    let candidates = body.get("model").and_then(Value::as_str).into_iter().chain(
        body.get("models")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str),
    );
    for model in candidates {
        let record = task
            .resolved_context_capabilities
            .iter()
            .find(|c| c.model_id == model);
        hard = hard.min(
            record
                .and_then(|c| c.context_tokens)
                .filter(|n| *n > 0)
                .unwrap_or(UNKNOWN_CONTEXT_CEILING)
                .min(UNKNOWN_CONTEXT_CEILING),
        );
        if l.output_tokens
            > record
                .and_then(|c| c.output_tokens)
                .filter(|n| *n > 0)
                .unwrap_or(UNKNOWN_OUTPUT_CEILING)
        {
            bail!(
                "context_budget_exceeded: output allowance exceeds provider/fallback model ceiling"
            );
        }
    }
    if let Some(output) = body
        .get("max_tokens")
        .or_else(|| body.get("max_completion_tokens"))
        .or_else(|| {
            body.get("generationConfig")
                .and_then(|v| v.get("maxOutputTokens"))
        })
        && positive(output)? > l.output_tokens
    {
        bail!("context_budget_exceeded: serialized output limit exceeds reserved allowance");
    }
    let n = estimate_json(body)?;
    tracing::info!(
        estimator = "conservative_utf8_bytes_plus_framing",
        capability_basis = "untrusted_catalog_tighten_only",
        serialized_bytes = n - 256,
        input_estimate = n,
        input_limit = l.input_tokens,
        output_limit = l.output_tokens,
        model_context_limit = hard,
        "model context wire accounting"
    );
    if n > l.input_tokens || n.saturating_add(l.output_tokens) > hard {
        bail!("context_budget_exceeded: final serialized provider request cannot fit");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn task(context: Value) -> ControllerTask {
        let mut task = ControllerTask::from_value(
            &json!({"kind":"text.generate", "model":"deepseek/deepseek-v4.1-flash",
            "prompt":"LEGACY_ENVELOPE_DUPLICATE", "context":context}),
        )
        .unwrap();
        task.resolved_context_capabilities = vec![ModelCapabilityRecord {
            provider: "openrouter".into(),
            model_id: "deepseek/deepseek-v4.1-flash".into(),
            context_tokens: Some(1_000_000),
            output_tokens: Some(131_072),
            source: "synthetic_test_catalog".into(),
        }];
        task
    }
    #[test]
    fn exact_catalog_records_and_unknown_fallback_replace_family_guesses() {
        let catalog = parse_openrouter_capabilities(
            r#"[{"id":"exact/model","ctx":131072,"out":0.004},{"id":"duplicate","ctx":999999},{"id":"duplicate","ctx":65536},{"id":"invalid","ctx":-1}]"#,
        );
        assert!(!catalog.iter().any(|c| c.model_id == "duplicate"));
        assert_eq!(
            catalog[0].output_tokens, None,
            "out is pricing, not an output limit"
        );
        let mut t = task(json!({"active_turn":{"text":"now"}}));
        t.resolved_context_capabilities =
            resolve_capabilities("openrouter", vec!["exact/model".into()], &catalog);
        assert_eq!(limits(&t).unwrap().1, UNKNOWN_CONTEXT_CEILING);
        assert_eq!(limits(&t).unwrap().0.output_tokens, 4096);
        t.provider_options.insert("max_tokens".into(), json!(8192));
        assert!(
            prepare(&t).is_err(),
            "unreported output capability remains conservatively bounded"
        );
        t.provider_options.clear();
        t.resolved_context_capabilities = resolve_capabilities(
            "openrouter",
            vec!["exact/model".into(), "unknown/model".into()],
            &catalog,
        );
        let (l, bound) = limits(&t).unwrap();
        assert_eq!(bound, UNKNOWN_CONTEXT_CEILING);
        assert_eq!(l.input_tokens, UNKNOWN_CONTEXT_CEILING - 4096);
        assert_eq!(
            resolve_capabilities("gemini", vec!["exact/model".into()], &catalog)[0].context_tokens,
            None
        );
        assert!(parse_openrouter_capabilities("invalid-json").is_empty());
    }

    #[test]
    fn forged_positive_catalog_cannot_expand_but_smaller_fallback_tightens() {
        let catalog = parse_openrouter_capabilities(r#"[{"id":"forged/model","ctx":4000000}]"#);
        let mut t = task(json!({"active_turn":{"text":"current"}}));
        t.resolved_context_capabilities =
            resolve_capabilities("openrouter", vec!["forged/model".into()], &catalog);
        assert_eq!(limits(&t).unwrap().1, 16384);
        t.provider_options
            .insert("context_limits".into(), json!({"input_tokens":65536}));
        assert!(prepare(&t).is_err());
        t.provider_options.clear();
        t.resolved_context_capabilities[0].output_tokens = Some(100000);
        t.provider_options.insert("max_tokens".into(), json!(8192));
        assert!(prepare(&t).is_err());
        t.provider_options.clear();
        let smaller = parse_openrouter_capabilities(
            r#"[{"id":"exact/default","ctx":100000},{"id":"exact/fallback","ctx":8000}]"#,
        );
        t.resolved_context_capabilities = resolve_capabilities(
            "openrouter",
            vec!["exact/default".into(), "exact/fallback".into()],
            &smaller,
        );
        assert_eq!(limits(&t).unwrap().1, 8000);
        assert_eq!(limits(&t).unwrap().0.input_tokens, 3904);
    }

    #[test]
    fn incoming_capability_claims_cannot_expand_unknown_limits() {
        let t = ControllerTask::from_value(
            &json!({"kind":"text.generate","model":"deepseek/deepseek-v4.1-flash",
            "prompt":"current", "resolved_context_capabilities":[{"context_tokens":1000000}],
            "provider_options":{"model_capabilities":{"context_tokens":1000000}}}),
        )
        .unwrap();
        assert!(t.resolved_context_capabilities.is_empty());
        assert_eq!(limits(&t).unwrap().1, UNKNOWN_CONTEXT_CEILING);
    }

    #[test]
    fn canonical_renderer_and_repeated_prepare_do_not_inflate_context() {
        let t = task(json!({"identity":[{"text":"PERSONA_MARKER"}],
            "memory":[{"text":"RECALL_MARKER"}],"recalled_memory":[{"text":"RECALL_MARKER"}],
            "active_turn":{"role":"user","text":"CURRENT_MESSAGE"}}));
        let (a, _) = prepare(&t).unwrap();
        let (b, _) = prepare(&a).unwrap();
        let text = a.composed_prompt_text().unwrap();
        assert_eq!(text.matches("PERSONA_MARKER").count(), 1);
        assert_eq!(text.matches("RECALL_MARKER").count(), 1);
        assert_eq!(text.matches("CURRENT_MESSAGE").count(), 1);
        assert!(!text.contains("LEGACY_ENVELOPE_DUPLICATE"));
        assert_eq!(text, b.composed_prompt_text().unwrap());
        assert_eq!(
            t.context.memory.len(),
            1,
            "selection cannot rewrite original"
        );
    }
    #[test]
    fn recent_history_keeps_whole_pairs_and_leaves_source_intact() {
        let mut t = task(json!({"active_turn":{"text":"now"},"dialogue_window":[
            {"role":"user","text":"old-user"},{"role":"assistant","text":"old-reply"},
            {"role":"user","text":"new-user"},{"role":"assistant","text":"new-reply"}]}));
        t.provider_options
            .insert("context_limits".into(), json!({"history_tokens":150}));
        let (a, counts) = prepare(&t).unwrap();
        assert_eq!(counts.history_selected, 2);
        assert_eq!(counts.history_dropped, 2);
        assert_eq!(
            a.context.dialogue_window[0].text.as_deref(),
            Some("new-user")
        );
        assert_eq!(
            a.context.dialogue_window[1].text.as_deref(),
            Some("new-reply")
        );
        assert_eq!(t.context.dialogue_window.len(), 4);
    }
    #[test]
    fn tool_pair_drop_is_atomic_including_arguments() {
        let mut t = task(json!({"active_turn":{"text":"now"},"tool_history":[
            {"index":1,"tool_name":"older","arguments":{"oversize":"x".repeat(1000)},"result":"old"},
            {"index":2,"tool_name":"newer","arguments":{},"result":"new"}]}));
        t.provider_options
            .insert("context_limits".into(), json!({"tool_result_tokens":200}));
        let (a, c) = prepare(&t).unwrap();
        assert_eq!(c.tool_pairs_dropped, 1);
        assert_eq!(a.context.tool_history.len(), 1);
        assert_eq!(a.context.tool_history[0].tool_name, "newer");
        assert_eq!(a.context.tool_history[0].result, "new");
    }
    #[test]
    fn oversized_user_and_required_tool_schemas_fail_clearly() {
        let mut t = task(json!({"active_turn":{"text":"x".repeat(20000)}}));
        assert!(
            prepare(&t)
                .unwrap_err()
                .to_string()
                .contains("current user")
        );
        t.context.active_turn.as_mut().unwrap().text = Some("small".into());
        t.tools = vec![json!({"tool_name":"required","description":"x".repeat(10000)})];
        assert!(
            prepare(&t)
                .unwrap_err()
                .to_string()
                .contains("mandatory tool schemas")
        );
    }
    #[test]
    fn output_override_is_explicit_and_model_bounded() {
        let mut t = task(json!({"active_turn":{"text":"now"}}));
        let (a, _) = prepare(&t).unwrap();
        assert_eq!(a.provider_options["max_tokens"], 4096);
        t.provider_options.insert("max_tokens".into(), json!(2048));
        t.provider_options
            .insert("context_limits".into(), json!({"input_tokens":8192}));
        assert_eq!(prepare(&t).unwrap().0.provider_options["max_tokens"], 2048);
        t.provider_options
            .insert("context_limits".into(), json!({"input_tokens":1000000}));
        assert!(prepare(&t).is_err());
        t.provider_options.remove("context_limits");
        t.provider_options
            .insert("max_tokens".into(), json!(131073));
        assert!(prepare(&t).is_err());
        t.provider_options.insert("max_tokens".into(), json!(32768));
        t.provider_options
            .insert("max_completion_tokens".into(), json!(12));
        assert!(prepare(&t).is_err());
        for v in [json!(0), json!(-1), json!(1.5), json!("wrong")] {
            t.provider_options.remove("max_completion_tokens");
            t.provider_options.insert("max_tokens".into(), v);
            assert!(prepare(&t).is_err());
        }
    }
    #[test]
    fn oversized_newest_tool_result_fails_instead_of_disappearing() {
        let t = task(json!({"active_turn":{"text":"current"},"tool_history":[
            {"index":1,"tool_name":"required","arguments":{},"result":"x".repeat(20000)}]}));
        assert!(
            prepare(&t)
                .unwrap_err()
                .to_string()
                .contains("latest required tool result")
        );
        assert_eq!(t.context.tool_history.len(), 1);
    }

    #[test]
    fn unknown_model_cannot_assert_an_unverified_large_context_window() {
        let mut t = task(json!({"active_turn":{"text":"now"}}));
        t.model = Some("unverified-model".into());
        t.resolved_context_capabilities.clear();
        t.provider_options
            .insert("context_limits".into(), json!({"input_tokens":32768}));
        assert!(prepare(&t).is_err());
    }
    #[test]
    fn mandatory_instruction_overflow_is_never_silently_trimmed() {
        let t =
            task(json!({"active_turn":{"text":"now"},"instructions":[{"text":"x".repeat(40000)}]}));
        assert!(prepare(&t).unwrap_err().to_string().contains("mandatory"));
    }
    #[test]
    fn final_wire_budget_rejects_oversize_fallback_payload_and_unreserved_output() {
        let t = task(json!({"active_turn":{"text":"now"}}));
        assert!(
            account_wire(
                &json!({"model":"deepseek/deepseek-v4.1-flash",
            "models":["unverified-small-model"],"messages":[{"content":"x".repeat(17000)}]}),
                &t
            )
            .is_err()
        );
        assert!(
            account_wire(
                &json!({"model":"deepseek/deepseek-v4.1-flash","max_tokens":4097}),
                &t
            )
            .is_err()
        );
    }

    #[test]
    fn final_wire_budget_counts_json_escaping_at_exact_boundary() {
        let mut t = task(json!({"active_turn":{"text":"now"}}));
        let body = json!({"messages":[{"content":"\\\"\n".repeat(100)}]});
        let n = estimate_json(&body).unwrap();
        t.provider_options
            .insert("context_limits".into(), json!({"input_tokens":n}));
        account_wire(&body, &t).unwrap();
        t.provider_options
            .insert("context_limits".into(), json!({"input_tokens":n-1}));
        assert!(account_wire(&body, &t).is_err());
    }
    #[test]
    fn optional_context_is_trimmed_before_mandatory_content() {
        let mut t = task(
            json!({"active_turn":{"text":"CURRENT"},"instructions":[{"text":"REQUIRED"}],
            "recalled_memory":[{"text":"x".repeat(3000)}],
            "dialogue_window":[{"role":"user","text":"y".repeat(1000)},{"role":"assistant","text":"z".repeat(1000)}]}),
        );
        t.provider_options
            .insert("context_limits".into(), json!({"input_tokens":1000}));
        let (a, c) = prepare(&t).unwrap();
        assert!(c.history_dropped > 0);
        assert!(c.memory_dropped > 0);
        let text = a.composed_prompt_text().unwrap();
        assert!(text.contains("CURRENT"));
        assert!(text.contains("REQUIRED"));
    }
}
