use ansible_mesh_core::graph::LoopScript;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

/// In-progress state for a turn running under a LoopScript.
/// Stored in WorkingTurn.scripted_loop_context and round-tripped through
/// checkpoint_json so it survives approval-gate re-entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScriptedLoopExecutor {
    pub script: LoopScript,
    /// Index of the step currently being executed.
    pub step_cursor: usize,
    /// Index within the current tool_sequence step (which tool to dispatch next).
    pub tool_sequence_cursor: usize,
    /// Outputs keyed by step id, accumulated as steps complete.
    pub step_outputs: HashMap<String, Value>,
}

/// What the executor wants the runtime to do next.
#[derive(Debug)]
pub enum ScriptedLoopDecision {
    /// Invoke the model with the given phase label.
    EmitModelCall {
        phase: String,
        output_schema: Option<String>,
    },
    /// Park the turn and wait for operator approval.
    ParkForApproval {
        gate: String,
        surface_as: String,
        reject_action: String,
    },
    /// Dispatch the next tool in the tool_sequence.
    ExecuteNextTool {
        tool_name: String,
        arguments: Value,
    },
    /// All tools in the current tool_sequence step are done — record and advance.
    ToolSequenceComplete,
    /// Force a SyncApartment checkpoint.
    ForceCheckpoint,
    /// The entire script is complete.
    Complete { final_output: Value },
    /// An unrecoverable error — fail the turn.
    Fail { reason: String },
}

impl ScriptedLoopExecutor {
    pub fn new(script: LoopScript) -> Self {
        Self {
            script,
            step_cursor: 0,
            tool_sequence_cursor: 0,
            step_outputs: HashMap::new(),
        }
    }

    /// Record the output of the current step and advance the cursor to the next step.
    pub fn record_step_output(&mut self, output: Value) {
        if let Some(step) = self.script.steps.get(self.step_cursor) {
            self.step_outputs.insert(step.id.clone(), output);
        }
        self.step_cursor += 1;
        self.tool_sequence_cursor = 0;
    }

    /// Advance the tool_sequence_cursor after a tool result arrives.
    pub fn advance_tool_cursor(&mut self) {
        self.tool_sequence_cursor += 1;
    }

    /// Accumulate a tool result into the current tool_sequence step's output array.
    pub fn record_tool_result(&mut self, result: Value) {
        if let Some(step) = self.script.steps.get(self.step_cursor) {
            let entry = self
                .step_outputs
                .entry(step.id.clone())
                .or_insert_with(|| Value::Array(vec![]));
            if let Value::Array(arr) = entry {
                arr.push(result);
            }
        }
    }

    /// The id of the step currently executing.
    pub fn current_step_id(&self) -> Option<&str> {
        self.script.steps.get(self.step_cursor).map(|s| s.id.as_str())
    }

    /// The type of the step currently executing.
    pub fn current_step_type(&self) -> Option<&str> {
        self.script.steps.get(self.step_cursor).map(|s| s.step_type.as_str())
    }

    /// Decide what the runtime should do for the current step.
    pub fn advance(&self) -> ScriptedLoopDecision {
        let Some(step) = self.script.steps.get(self.step_cursor) else {
            let final_output = self
                .script
                .steps
                .last()
                .and_then(|s| self.step_outputs.get(&s.id))
                .cloned()
                .unwrap_or(Value::Null);
            return ScriptedLoopDecision::Complete { final_output };
        };

        match step.step_type.as_str() {
            "model_call" => {
                let phase = step
                    .config
                    .get("phase")
                    .and_then(Value::as_str)
                    .unwrap_or("respond")
                    .to_string();
                let output_schema = step
                    .config
                    .get("output_schema")
                    .and_then(Value::as_str)
                    .map(String::from);
                ScriptedLoopDecision::EmitModelCall { phase, output_schema }
            }

            "approval_gate" => {
                let gate = step
                    .config
                    .get("gate")
                    .and_then(Value::as_str)
                    .unwrap_or("operator")
                    .to_string();
                let surface_as = step
                    .config
                    .get("surface_as")
                    .and_then(Value::as_str)
                    .unwrap_or("raw")
                    .to_string();
                let reject_action = step
                    .config
                    .get("reject_action")
                    .and_then(Value::as_str)
                    .unwrap_or("fail_turn")
                    .to_string();
                ScriptedLoopDecision::ParkForApproval {
                    gate,
                    surface_as,
                    reject_action,
                }
            }

            "tool_sequence" => {
                let input_path = step.input.as_deref().unwrap_or("");
                let abort_on_mismatch = step
                    .config
                    .get("abort_on_mismatch")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);

                let tools_val = self.resolve_input(input_path);
                let tools_array = match tools_val.as_ref().and_then(Value::as_array) {
                    Some(arr) => arr,
                    None => {
                        return ScriptedLoopDecision::Fail {
                            reason: format!(
                                "tool_sequence input '{}' did not resolve to an array",
                                input_path
                            ),
                        };
                    }
                };

                let Some(tool_step) = tools_array.get(self.tool_sequence_cursor) else {
                    // All tools dispatched — signal completion of this step.
                    return ScriptedLoopDecision::ToolSequenceComplete;
                };

                let tool_name = match tool_step.get("tool_name").and_then(Value::as_str) {
                    Some(n) => n.to_string(),
                    None => {
                        let reason = format!(
                            "tool_sequence step {} missing 'tool_name'",
                            self.tool_sequence_cursor
                        );
                        if abort_on_mismatch {
                            return ScriptedLoopDecision::Fail { reason };
                        }
                        return ScriptedLoopDecision::Fail { reason };
                    }
                };

                let arguments = tool_step
                    .get("arguments")
                    .cloned()
                    .unwrap_or_else(|| Value::Object(Default::default()));

                ScriptedLoopDecision::ExecuteNextTool { tool_name, arguments }
            }

            "checkpoint" => ScriptedLoopDecision::ForceCheckpoint,

            other => ScriptedLoopDecision::Fail {
                reason: format!("unknown loop step type: '{}'", other),
            },
        }
    }

    /// Resolve a dot-path (e.g. "plan", "plan.steps") against accumulated step_outputs.
    pub fn resolve_input(&self, path: &str) -> Option<Value> {
        if path.is_empty() {
            return None;
        }
        let (root, rest) = match path.split_once('.') {
            Some((r, s)) => (r, Some(s)),
            None => (path, None),
        };
        let root_val = self.step_outputs.get(root)?;
        match rest {
            None => Some(root_val.clone()),
            Some(subpath) => resolve_json_path(root_val, subpath),
        }
    }
}

fn resolve_json_path(val: &Value, path: &str) -> Option<Value> {
    let (key, rest) = match path.split_once('.') {
        Some((k, r)) => (k, Some(r)),
        None => (path, None),
    };
    let child = match val {
        Value::Object(map) => map.get(key)?,
        Value::Array(arr) => {
            let idx: usize = key.parse().ok()?;
            arr.get(idx)?
        }
        _ => return None,
    };
    match rest {
        None => Some(child.clone()),
        Some(subpath) => resolve_json_path(child, subpath),
    }
}

/// Validate that a Value conforms to the plan_proposal schema.
/// Returns the extracted steps on success or an error string.
pub fn validate_plan_proposal(value: &Value) -> Result<Vec<Value>, String> {
    let steps = value
        .get("steps")
        .and_then(Value::as_array)
        .ok_or_else(|| "plan_proposal missing required 'steps' array".to_string())?;
    if steps.is_empty() {
        return Err("plan_proposal 'steps' array is empty".to_string());
    }
    for (i, step) in steps.iter().enumerate() {
        if step.get("tool_name").and_then(Value::as_str).is_none() {
            return Err(format!(
                "plan_proposal steps[{}] missing required 'tool_name'",
                i
            ));
        }
    }
    Ok(steps.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ansible_mesh_core::graph::{LoopScript, LoopStep};
    use serde_json::json;

    fn plan_execute_script() -> LoopScript {
        LoopScript {
            variant: "plan_execute".to_string(),
            steps: vec![
                LoopStep {
                    id: "plan".to_string(),
                    step_type: "model_call".to_string(),
                    input: None,
                    config: json!({"phase": "plan", "output_schema": "plan_proposal"}),
                },
                LoopStep {
                    id: "approve".to_string(),
                    step_type: "approval_gate".to_string(),
                    input: Some("plan".to_string()),
                    config: json!({"gate": "operator", "surface_as": "plan_card", "reject_action": "fail_turn"}),
                },
                LoopStep {
                    id: "execute".to_string(),
                    step_type: "tool_sequence".to_string(),
                    input: Some("plan.steps".to_string()),
                    config: json!({"abort_on_mismatch": true}),
                },
                LoopStep {
                    id: "synthesize".to_string(),
                    step_type: "model_call".to_string(),
                    input: Some("execute".to_string()),
                    config: json!({"phase": "synthesize"}),
                },
            ],
        }
    }

    #[test]
    fn step_0_emits_plan_model_call() {
        let exec = ScriptedLoopExecutor::new(plan_execute_script());
        assert!(matches!(
            exec.advance(),
            ScriptedLoopDecision::EmitModelCall { ref phase, .. } if phase == "plan"
        ));
    }

    #[test]
    fn step_1_parks_for_approval() {
        let mut exec = ScriptedLoopExecutor::new(plan_execute_script());
        exec.record_step_output(json!({
            "objective": "read a file",
            "steps": [{"tool_name": "workspace.read_file", "arguments": {"path": "/tmp/x"}}]
        }));
        assert!(matches!(
            exec.advance(),
            ScriptedLoopDecision::ParkForApproval { ref gate, .. } if gate == "operator"
        ));
    }

    #[test]
    fn step_2_dispatches_first_tool() {
        let mut exec = ScriptedLoopExecutor::new(plan_execute_script());
        exec.record_step_output(json!({
            "objective": "read a file",
            "steps": [{"tool_name": "workspace.read_file", "arguments": {"path": "/tmp/x"}}]
        }));
        exec.record_step_output(json!({"approved": true}));
        assert!(matches!(
            exec.advance(),
            ScriptedLoopDecision::ExecuteNextTool { ref tool_name, .. } if tool_name == "workspace.read_file"
        ));
    }

    #[test]
    fn tool_sequence_complete_after_all_tools() {
        let mut exec = ScriptedLoopExecutor::new(plan_execute_script());
        exec.record_step_output(json!({
            "steps": [{"tool_name": "echo", "arguments": {}}]
        }));
        exec.record_step_output(json!({"approved": true}));
        // Advance tool cursor past the only tool
        exec.advance_tool_cursor();
        assert!(matches!(exec.advance(), ScriptedLoopDecision::ToolSequenceComplete));
    }

    #[test]
    fn complete_after_all_steps() {
        let mut exec = ScriptedLoopExecutor::new(plan_execute_script());
        exec.record_step_output(json!({"steps": [{"tool_name": "echo", "arguments": {}}]}));
        exec.record_step_output(json!({"approved": true}));
        exec.record_step_output(json!([{"tool_name": "echo", "ok": true}]));
        exec.record_step_output(json!({"content": "done"}));
        assert!(matches!(exec.advance(), ScriptedLoopDecision::Complete { .. }));
    }

    #[test]
    fn resolve_nested_dot_path() {
        let mut exec = ScriptedLoopExecutor::new(plan_execute_script());
        exec.step_outputs.insert(
            "plan".to_string(),
            json!({"steps": [{"tool_name": "echo"}]}),
        );
        let resolved = exec.resolve_input("plan.steps");
        assert!(resolved.is_some());
        assert_eq!(resolved.unwrap().as_array().unwrap().len(), 1);
    }

    #[test]
    fn validate_plan_proposal_ok() {
        let v = json!({"objective": "test", "steps": [{"tool_name": "echo", "arguments": {}}]});
        assert!(validate_plan_proposal(&v).is_ok());
    }

    #[test]
    fn validate_plan_proposal_missing_steps() {
        let v = json!({"objective": "test"});
        assert!(validate_plan_proposal(&v).is_err());
    }

    #[test]
    fn validate_plan_proposal_empty_steps() {
        let v = json!({"steps": []});
        assert!(validate_plan_proposal(&v).is_err());
    }

    #[test]
    fn validate_plan_proposal_missing_tool_name() {
        let v = json!({"steps": [{"arguments": {}}]});
        assert!(validate_plan_proposal(&v).is_err());
    }

    #[test]
    fn executor_roundtrips_through_json() {
        let exec = ScriptedLoopExecutor::new(plan_execute_script());
        let serialized = serde_json::to_value(&exec).unwrap();
        let deserialized: ScriptedLoopExecutor = serde_json::from_value(serialized).unwrap();
        assert_eq!(deserialized.step_cursor, 0);
        assert_eq!(deserialized.script.variant, "plan_execute");
    }
}
