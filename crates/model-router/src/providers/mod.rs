mod anthropic;
pub mod decisions;
mod elevenlabs;
mod gemini;
pub mod mlx;
pub mod ollama;
pub mod onnx;
mod openai;
pub mod parakeet;

pub use anthropic::AnthropicProvider;
pub use decisions::DecisionsProvider;
pub use elevenlabs::ElevenLabsProvider;
pub use gemini::{GeminiAuth, GeminiProvider};
pub use mlx::MlxProvider;
pub use ollama::OllamaProvider;
pub use onnx::OnnxProvider;
pub use openai::OpenAIProvider;
pub use parakeet::ParakeetProvider;

/// Whether a text task asked for structured reply channels (voice text,
/// memory, or a plan) that the model must return inside one JSON object.
pub(crate) fn wants_structured_reply(task: &crate::controller::ControllerTask) -> bool {
    task.kind == crate::controller::TaskKind::TextGenerate
        && (task.wants_channel("spoken_text")
            || task.wants_channel("memory_concept")
            || task.wants_channel("active_plan"))
}

/// The structured-output contract sent to chat models as a system prompt.
/// Without it a model asked (by the turn loop) to "include an active_plan"
/// improvises the format — newer OpenRouter models wrote it into the reply as
/// a markdown section the user saw (DEF-219). Tools stay native; only the
/// text-reply shape is constrained. Shared by the Anthropic and
/// OpenAI-compatible (OpenRouter) providers.
pub(crate) fn structured_reply_contract(
    task: &crate::controller::ControllerTask,
) -> Option<String> {
    if !wants_structured_reply(task) {
        return None;
    }

    let memory_instruction = if task.wants_channel("memory_concept") {
        " If — and only if — this exchange contains something genuinely worth remembering \
         (a user preference, a decision made, a fact learned, or a pattern worth recalling \
         later), include \"memory_candidate\" with fields: \"concept\" (short kebab-case \
         slug), \"content\" (one or two sentences distilling what is worth keeping), and \
         optional \"tags\" (array of short strings). Omit memory_candidate entirely for \
         routine exchanges, simple questions, greetings, or transient state."
    } else {
        ""
    };
    let plan_instruction = if task.wants_channel("active_plan") {
        " When working a multi-step task, also include \"active_plan\" in that JSON object: \
         {\"goal\": string, \"status\": string, \"steps\": [{\"id\": integer, \
         \"description\": string, \"tool_name\": string, \"status\": string}]}. Omit \
         active_plan for single-step exchanges. Never write the plan into display_text."
    } else {
        ""
    };

    let tool_clause = if task.tools.is_empty() {
        ""
    } else {
        "When a tool is needed, call one of the declared tools natively — do not write a \
         JSON tool_call object by hand. Use tool input fields exactly as declared and \
         include every required field.\n"
    };

    Some(format!(
        "{}When replying with text, reply with ONLY a raw JSON object — no markdown code \
         fences, no text outside the JSON — containing \"display_text\" (your reply, \
         markdown fine) and \"spoken_text\" (conversational version for voice, no \
         markdown).{}{}",
        tool_clause, memory_instruction, plan_instruction,
    ))
}

/// Split a model reply into its structured JSON object and any prose after it.
///
/// Structured cognitive replies are asked for as one JSON object, but newer
/// models (glm-5.3, deepseek-v4.1 via OpenRouter) often emit the object —
/// typically `{"active_plan": …}` — and then the user-facing answer as plain
/// text. Parsing only the whole reply as JSON left the plan object in the
/// message the user saw (DEF-219). Returns the object and the trimmed
/// remainder (empty when the reply was only the object). `None` when the reply
/// does not start with a JSON object at all.
pub(crate) fn split_structured_reply(content: &str) -> Option<(serde_json::Value, String)> {
    let body = content.trim();
    if let Some(rest) = body.strip_prefix("```") {
        // Optional ```json fence around the object.
        let rest = rest.strip_prefix("json").unwrap_or(rest);
        let end = rest.find("```")?;
        let value = serde_json::from_str::<serde_json::Value>(rest[..end].trim()).ok()?;
        let after = rest[end + 3..].trim();
        return value.is_object().then(|| (value, after.to_string()));
    }
    if !body.starts_with('{') {
        return None;
    }
    let mut stream = serde_json::Deserializer::from_str(body).into_iter::<serde_json::Value>();
    let value = stream.next()?.ok()?;
    if !value.is_object() {
        return None;
    }
    Some((value, body[stream.byte_offset()..].trim().to_string()))
}

#[cfg(test)]
mod split_structured_reply_tests {
    use super::split_structured_reply;
    use serde_json::json;

    #[test]
    fn whole_object_has_empty_remainder() {
        let (value, rest) = split_structured_reply(r#" {"display_text":"hi"} "#).unwrap();
        assert_eq!(value, json!({"display_text": "hi"}));
        assert!(rest.is_empty());
    }

    #[test]
    fn plan_object_then_prose_is_split() {
        let raw =
            "{\"active_plan\":{\"goal\":\"g\",\"steps\":[]}}\n\nAll good — the fix is working.";
        let (value, rest) = split_structured_reply(raw).unwrap();
        assert_eq!(value, json!({"active_plan": {"goal": "g", "steps": []}}));
        assert_eq!(rest, "All good — the fix is working.");
    }

    #[test]
    fn fenced_object_then_prose_is_split() {
        let raw = "```json\n{\"active_plan\":{\"goal\":\"g\"}}\n```\nDone.";
        let (value, rest) = split_structured_reply(raw).unwrap();
        assert_eq!(value, json!({"active_plan": {"goal": "g"}}));
        assert_eq!(rest, "Done.");
    }

    #[test]
    fn plain_prose_and_non_objects_are_not_structured() {
        assert!(split_structured_reply("Hello {not json} there").is_none());
        assert!(split_structured_reply("[1,2,3] trailing").is_none());
        assert!(split_structured_reply("{broken json").is_none());
    }
}
