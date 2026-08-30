//! Scripted hotel sensors — a conditional cron job kind.
//!
//! See `doc:scripted-hotel-sensors` (intel-graph proposal). A `CronJob` whose
//! `target_role` is [`CRON_TARGET_ROLE`] is intercepted by
//! `CronTicker::fire()` in-process, exactly like `memory.hygiene`,
//! dream-sweep, and autonomy-sweep. Its `payload` names a `sensor_id`; the
//! actual check logic is a Rhai script stored as an ordinary graph config
//! value (`sensor_script:<sensor_id>`, same `NODE_KIND_CONFIG` storage
//! `heartbeat_chat_id` already uses) — a script ships with **no Rust change
//! and no deploy**.
//!
//! Unlike `memory.hygiene`/`dream-sweep` there is no opt-in flag: mesh
//! `CronJobSync` may replicate a sensor's `CronJob` *definition* to every
//! peer hotel, but the script itself is per-hotel config that is never
//! mesh-synced. A hotel with no matching `sensor_script:<id>` row simply has
//! nothing to run — the natural per-hotel gate is script presence, not a
//! separate enabled-locally flag.
//!
//! ## Slice 1 scope
//!
//! `query_local` and `deliver` are fully wired. `query_remote` (the mesh
//! LifeGraph over bolt/cypher) is deliberately **not** registered yet — this
//! hotel process (`aiua`) has no Memgraph client today; only the
//! `data-memorygraphrag` guest does. Proxying a remote query through that
//! guest over IPC is real design work (a synchronous-shaped request/response
//! over an async inbox), tracked as a follow-up rather than guessed at here.
//! `investigate` is registered but returns an error — the paracrine/whisper
//! lookaside dispatch it should ride is a separate wiring task.

use ansible_mesh_core::domain::GraphDomain;
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use tracing::{debug, warn};

/// Sentinel `target_role` a `CronJob` uses to mark itself as a scripted
/// sensor rather than a normal role-delivery job. Never resolves to a guest
/// inbox — `CronTicker::fire()` intercepts it before reaching the normal
/// delivery path.
pub const CRON_TARGET_ROLE: &str = "sensor:script";

fn config_key(sensor_id: &str) -> String {
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
    /// `life.patch.apply` already requires for proposed writes. Whether this
    /// needs UI/ceremony beyond "an operator or an agent with standing
    /// approval authority wrote this row" is an open question on the
    /// proposal, not decided here.
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

impl SensorScript {
    pub fn load(graph: &GraphDomain, sensor_id: &str) -> anyhow::Result<Option<Self>> {
        match graph.get_config_value(&config_key(sensor_id))? {
            None => Ok(None),
            Some(raw) => Ok(serde_json::from_str(&raw)?),
        }
    }

    pub fn save(&self, graph: &GraphDomain) -> anyhow::Result<()> {
        graph.set_config_value(&config_key(&self.id), &serde_json::to_string(self)?)
    }
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
    /// dispatch path — see module docs.
    Investigate { target_role: String, brief: String },
}

/// Runs `script.source`, returning the verdict it decided. Errors are the
/// script's own (a Rhai parse/eval error, or a script-thrown error from a
/// registered function) — the caller logs and treats them as `Quiet` rather
/// than letting a bad script wedge the cron tick.
///
/// Deliberately synchronous and confined to no `.await` points: the Rhai
/// `Engine` and its captured `Rc<RefCell<_>>` verdict cell are not
/// `Send`, so this must fully resolve to a plain `SensorVerdict` before the
/// caller does anything async with it (the enclosing cron tick future must
/// stay `Send` for `tokio::spawn`).
pub fn run_script(
    graph: &Arc<GraphDomain>,
    script: &SensorScript,
) -> anyhow::Result<SensorVerdict> {
    let verdict: Rc<RefCell<SensorVerdict>> = Rc::new(RefCell::new(SensorVerdict::Quiet));

    let mut engine = rhai::Engine::new();

    // query_local(sql) -> array of maps. Read-only: local storage is
    // key/value config plus typed accessors on GraphDomain, not a general
    // query engine, so this is deliberately narrow — a single config read,
    // not arbitrary SQL. Broaden when a sensor actually needs list/filter.
    let graph_for_query = Arc::clone(graph);
    engine.register_fn("config_value", move |key: &str| -> String {
        graph_for_query
            .get_config_value(key)
            .ok()
            .flatten()
            .unwrap_or_default()
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

    engine.register_fn(
        "investigate",
        |_target_role: &str, _brief: &str| -> Result<(), Box<rhai::EvalAltResult>> {
            Err("investigate() is not wired to a dispatch path yet — see doc:scripted-hotel-sensors"
                .into())
        },
    );

    // rhai's error type carries `Rc`-backed variants and is neither `Send`
    // nor `Sync`, so it can't convert via `anyhow`'s blanket `From` impl —
    // stringify it before it crosses that boundary.
    engine
        .eval::<()>(&script.source)
        .map_err(|e| anyhow::anyhow!("rhai eval error: {e}"))?;

    let result = verdict.borrow().clone();
    Ok(result)
}

/// Loads and runs the named sensor. `Ok(None)` means there is nothing
/// locally registered for this id — the natural per-hotel gate (see module
/// docs), not an error.
pub fn evaluate(
    graph: &Arc<GraphDomain>,
    sensor_id: &str,
) -> anyhow::Result<Option<SensorVerdict>> {
    let Some(mut script) = SensorScript::load(graph, sensor_id)? else {
        debug!(
            sensor_id,
            "sensor_scripts: no script registered locally — skipping"
        );
        return Ok(None);
    };
    if !script.enabled {
        debug!(sensor_id, "sensor_scripts: script disabled — skipping");
        return Ok(None);
    }
    if !script.operator_approved {
        warn!(
            sensor_id,
            "sensor_scripts: script not operator_approved — refusing to run"
        );
        return Ok(None);
    }

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);

    let outcome = run_script(graph, &script);
    script.last_run_at = Some(now_ms);
    script.last_result = Some(match &outcome {
        Ok(SensorVerdict::Quiet) => "quiet".to_string(),
        Ok(SensorVerdict::Deliver { .. }) => "delivered".to_string(),
        Ok(SensorVerdict::Investigate { .. }) => "investigate".to_string(),
        Err(e) => format!("error: {e}"),
    });
    if let Err(e) = script.save(graph) {
        warn!(
            sensor_id,
            "sensor_scripts: failed to persist last_result: {e}"
        );
    }

    match outcome {
        Ok(verdict) => Ok(Some(verdict)),
        Err(e) => {
            warn!(sensor_id, "sensor_scripts: script error (non-fatal): {e}");
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ansible_mesh_core::sqlite_storage::SqliteGraphStorage;

    fn test_graph() -> Arc<GraphDomain> {
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        Arc::new(GraphDomain::new(Arc::new(graph_store.adapter())))
    }

    #[test]
    fn quiet_script_yields_quiet_verdict() {
        let graph = test_graph();
        let script = SensorScript {
            id: "quiet-test".into(),
            source: "// nothing to do".into(),
            enabled: true,
            operator_approved: true,
            last_run_at: None,
            last_result: None,
        };
        let verdict = run_script(&graph, &script).unwrap();
        assert_eq!(verdict, SensorVerdict::Quiet);
    }

    #[test]
    fn deliver_call_yields_deliver_verdict() {
        let graph = test_graph();
        let script = SensorScript {
            id: "deliver-test".into(),
            source: r#"deliver("role:agent-beacon:orchestrator", "hello")"#.into(),
            enabled: true,
            operator_approved: true,
            last_run_at: None,
            last_result: None,
        };
        let verdict = run_script(&graph, &script).unwrap();
        assert_eq!(
            verdict,
            SensorVerdict::Deliver {
                target_role: "role:agent-beacon:orchestrator".into(),
                message: "hello".into(),
            }
        );
    }

    #[test]
    fn config_value_reads_hotel_config() {
        let graph = test_graph();
        graph
            .set_config_value("probe_key", "\"probe_value\"")
            .unwrap();
        let script = SensorScript {
            id: "config-test".into(),
            source: r#"
                let v = config_value("probe_key");
                deliver("role:x", v);
            "#
            .into(),
            enabled: true,
            operator_approved: true,
            last_run_at: None,
            last_result: None,
        };
        let verdict = run_script(&graph, &script).unwrap();
        match verdict {
            SensorVerdict::Deliver { message, .. } => assert!(message.contains("probe_value")),
            other => panic!("expected Deliver, got {other:?}"),
        }
    }

    #[test]
    fn investigate_errors_until_wired() {
        let graph = test_graph();
        let script = SensorScript {
            id: "investigate-test".into(),
            source: r#"investigate("role:x", "brief")"#.into(),
            enabled: true,
            operator_approved: true,
            last_run_at: None,
            last_result: None,
        };
        assert!(run_script(&graph, &script).is_err());
    }

    #[test]
    fn unapproved_script_does_not_run() {
        let graph = test_graph();
        let script = SensorScript {
            id: "unapproved-test".into(),
            source: r#"deliver("role:x", "should not fire")"#.into(),
            enabled: true,
            operator_approved: false,
            last_run_at: None,
            last_result: None,
        };
        script.save(&graph).unwrap();
        let verdict = evaluate(&graph, "unapproved-test").unwrap();
        assert_eq!(verdict, None);
    }

    #[test]
    fn missing_script_is_a_quiet_skip_not_an_error() {
        let graph = test_graph();
        let verdict = evaluate(&graph, "does-not-exist").unwrap();
        assert_eq!(verdict, None);
    }
}
