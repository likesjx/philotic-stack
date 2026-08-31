//! Scripted hotel sensors — the Rhai check engine.
//!
//! See `docs/architecture/SCRIPTED_HOTEL_SENSORS_PROPOSAL.md`. A sensor is a
//! small Rhai script, stored as hotel config (`sensor_script:<sensor_id>`,
//! same storage `heartbeat_chat_id` already uses), that decides on a cron
//! tick whether there is real work. This module owns only the engine and the
//! data shape — it is deliberately IPC-free and Memgraph-free so it stays
//! portable. `sensor_provider::SensorProvider` (this crate's bin target)
//! supplies `config_value`/`life_call` as closures that do the actual IPC or
//! in-process `LifeGraphProvider::invoke` work, bridging their async calls
//! synchronously via `tokio::task::block_in_place` before this engine ever
//! runs — `run_script` itself must stay fully synchronous: the Rhai
//! `Engine` and its captured `Rc<RefCell<_>>` verdict cell are not `Send`.
//!
//! This lives in `data-memorygraphrag`, not `aiua`, because the interesting
//! capability a sensor needs — reading the mesh LifeGraph — is only
//! available in-process here (`LifeGraphProvider::invoke`, zero IPC hop). A
//! sensor `CronJob` is an ordinary `target_role: "life-graph-runner"`
//! delivery; no sentinel intercept is needed in `aiua`'s `CronTicker`.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::cell::RefCell;
use std::rc::Rc;

/// `DatasourceTask.kind` a scripted-sensor run arrives as.
pub const SENSOR_TASK_KIND: &str = "sensor.run";

pub fn config_key(sensor_id: &str) -> String {
    format!("sensor_script:{sensor_id}")
}

/// A scripted sensor definition, stored as an ordinary hotel config value.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SensorScript {
    pub id: String,
    /// Rhai source.
    pub source: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Governance gate — a script is graph-editable, so it needs the same
    /// "someone with authority signed off" gate LifeGraph's
    /// `life.patch.apply` already requires for proposed writes.
    #[serde(default)]
    pub operator_approved: bool,
    #[serde(default)]
    pub last_run_at: Option<u64>,
    /// Human-readable outcome of the last run — "quiet", "delivered", or an
    /// error string. Observability only; not used for any decision.
    #[serde(default)]
    pub last_result: Option<String>,
}

fn default_true() -> bool {
    true
}

/// What a sensor script decided this tick.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SensorVerdict {
    /// Nothing to do.
    Quiet,
    /// Pure delivery — pre-formatted text; the agent just relays it.
    Deliver {
        target_role: String,
        message: String,
    },
    /// Hand a finding to a philote for actual reasoning. Not yet wired to a
    /// dispatch path — see module docs on the proposal.
    Investigate { target_role: String, brief: String },
}

/// Runs `source`, returning the verdict it decided. Errors are the script's
/// own (a Rhai parse/eval error, a script-thrown error, or an error
/// surfaced from `config_value`/`life_call`) — the caller logs and treats
/// them as `Quiet` rather than letting a bad script wedge the cron tick.
pub fn run_script(
    source: &str,
    config_value: impl Fn(&str) -> String + 'static,
    life_call: impl Fn(&str, Value) -> Result<Value, String> + 'static,
) -> anyhow::Result<SensorVerdict> {
    let verdict: Rc<RefCell<SensorVerdict>> = Rc::new(RefCell::new(SensorVerdict::Quiet));

    let mut engine = rhai::Engine::new();

    engine.register_fn("config_value", move |key: &str| -> String {
        config_value(key)
    });

    engine.register_fn("now_iso", || -> String {
        chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
    });

    engine.register_fn("operator_local", |iso: &str, tz: &str| -> String {
        let Ok(zone) = tz.parse::<chrono_tz::Tz>() else {
            return iso.to_string();
        };
        chrono::DateTime::parse_from_rfc3339(&iso.replace('Z', "+00:00"))
            .map(|dt| {
                dt.with_timezone(&zone)
                    .format("%Y-%m-%d %-I:%M %p %Z")
                    .to_string()
            })
            .unwrap_or_else(|_| iso.to_string())
    });

    let verdict_for_deliver = Rc::clone(&verdict);
    engine.register_fn("deliver", move |target_role: &str, message: &str| {
        *verdict_for_deliver.borrow_mut() = SensorVerdict::Deliver {
            target_role: target_role.to_string(),
            message: message.to_string(),
        };
    });

    let verdict_for_investigate = Rc::clone(&verdict);
    engine.register_fn("investigate", move |target_role: &str, brief: &str| {
        *verdict_for_investigate.borrow_mut() = SensorVerdict::Investigate {
            target_role: target_role.to_string(),
            brief: brief.to_string(),
        };
    });

    // life_call(tool, args) -> result. The entire life.* surface
    // (`LifeGraphProvider::invoke`'s dispatch table), scriptable in-process.
    engine.register_fn(
        "life_call",
        move |tool: &str, args: rhai::Dynamic| -> Result<rhai::Dynamic, Box<rhai::EvalAltResult>> {
            let params: Value = rhai::serde::from_dynamic(&args)
                .map_err(|e| format!("life_call: bad args: {e}"))?;
            let result = life_call(tool, params).map_err(|e| format!("life_call({tool}): {e}"))?;
            rhai::serde::to_dynamic(&result)
                .map_err(|e| format!("life_call: unrepresentable result: {e}").into())
        },
    );

    // rhai's error type carries `Rc`-backed variants and is neither `Send`
    // nor `Sync`, so it can't convert via `anyhow`'s blanket `From` impl —
    // stringify it before it crosses that boundary.
    engine
        .eval::<()>(source)
        .map_err(|e| anyhow::anyhow!("rhai eval error: {e}"))?;

    let result = verdict.borrow().clone();
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_config(_key: &str) -> String {
        String::new()
    }

    fn no_life_call(_tool: &str, _args: Value) -> Result<Value, String> {
        Err("life_call not available in this test".to_string())
    }

    #[test]
    fn quiet_script_yields_quiet_verdict() {
        let verdict = run_script("// nothing to do", no_config, no_life_call).unwrap();
        assert_eq!(verdict, SensorVerdict::Quiet);
    }

    #[test]
    fn deliver_call_yields_deliver_verdict() {
        let verdict = run_script(
            r#"deliver("role:agent-beacon:orchestrator", "hello")"#,
            no_config,
            no_life_call,
        )
        .unwrap();
        assert_eq!(
            verdict,
            SensorVerdict::Deliver {
                target_role: "role:agent-beacon:orchestrator".into(),
                message: "hello".into(),
            }
        );
    }

    #[test]
    fn investigate_call_yields_investigate_verdict() {
        let verdict = run_script(
            r#"investigate("role:agent-beacon:orchestrator", "stale open loop")"#,
            no_config,
            no_life_call,
        )
        .unwrap();
        assert_eq!(
            verdict,
            SensorVerdict::Investigate {
                target_role: "role:agent-beacon:orchestrator".into(),
                brief: "stale open loop".into(),
            }
        );
    }

    #[test]
    fn config_value_reads_through_closure() {
        let verdict = run_script(
            r#"
                let v = config_value("probe_key");
                deliver("role:x", v);
            "#,
            |key| {
                if key == "probe_key" {
                    "probe_value".to_string()
                } else {
                    String::new()
                }
            },
            no_life_call,
        )
        .unwrap();
        match verdict {
            SensorVerdict::Deliver { message, .. } => assert_eq!(message, "probe_value"),
            other => panic!("expected Deliver, got {other:?}"),
        }
    }

    #[test]
    fn life_call_round_trips_through_closure() {
        let verdict = run_script(
            r#"
                let result = life_call("life.recall", #{ query: "overdue" });
                deliver("role:x", result.status);
            "#,
            no_config,
            |tool, params| {
                assert_eq!(tool, "life.recall");
                assert_eq!(params["query"], "overdue");
                Ok(serde_json::json!({"status": "ok"}))
            },
        )
        .unwrap();
        match verdict {
            SensorVerdict::Deliver { message, .. } => assert_eq!(message, "ok"),
            other => panic!("expected Deliver, got {other:?}"),
        }
    }

    #[test]
    fn life_call_error_propagates_as_script_error() {
        let err = run_script(
            r#"life_call("life.recall", #{})"#,
            no_config,
            |_tool, _params| Err("memgraph unreachable".to_string()),
        )
        .unwrap_err();
        assert!(format!("{err}").contains("memgraph unreachable"));
    }
}
