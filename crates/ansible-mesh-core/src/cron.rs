//! Cron job types and schedule computation for the hotel cron subsystem.

use anyhow::{Context, Result};
use chrono::{TimeZone, Utc};
use cron::Schedule;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::str::FromStr;

/// Unique identifier for a cron job.
pub type CronJobId = String;

/// Who registered this cron job.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CronJobSource {
    Operator,
    Guest(String),
}

/// A scheduled envelope record stored in the hotel's Context Graph.
///
/// When `next_fire_at + cron_offset_ms <= now`, the `CronTicker` materialises
/// a `TaskInvoke` `EventEnvelope` with the `payload` (after `{timestamp}`
/// interpolation) and delivers it to `target_role`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronJob {
    /// Unique identifier (UUID string).
    pub id: CronJobId,

    /// Cron expression in 7-field format: `<sec> <min> <hour> <dom> <month> <dow> <year>`.
    /// Example: `"0 */5 * * * * *"` — every 5 minutes.
    pub schedule: String,

    /// Role inbox to deliver the trigger to.
    pub target_role: String,

    /// Target hotel node. `None` = local hotel (Slice 1 always uses local).
    pub target_node_id: Option<String>,

    /// Static JSON payload. `{timestamp}` is replaced with the `fire_epoch` (ms)
    /// at fire time. This is the only built-in template variable in Slice 1.
    pub payload: String,

    /// If true, guaranteed mesh-coordinated delivery (Slice 2+). Ignored in Slice 1.
    pub guaranteed: bool,

    /// Whether this job is currently active.
    pub enabled: bool,

    /// The `next_fire_at` value from the last successful fire (ms since epoch).
    pub last_fired_epoch: Option<u64>,

    /// Absolute next intended fire time (ms since epoch).
    pub next_fire_at: u64,

    /// Creation timestamp (ms since epoch).
    pub created_at: u64,

    /// Who registered this job.
    pub created_by: CronJobSource,
}

/// Variables available for payload template interpolation at fire time.
///
/// All `{var}` placeholders in a job's `payload` string are replaced with
/// the corresponding value before the envelope is dispatched.
pub struct CronInterpolationVars<'a> {
    /// Fire epoch in milliseconds since Unix epoch. Replaces `{timestamp}`.
    pub timestamp_ms: u64,
    /// ISO 8601 string of the fire time. Replaces `{iso_timestamp}`.
    pub iso_timestamp: String,
    /// The cron job's unique ID. Replaces `{job_id}`.
    pub job_id: &'a str,
    /// The firing hotel's node ID. Replaces `{node_id}`.
    pub node_id: &'a str,
    /// The destination role inbox. Replaces `{target_role}`.
    pub target_role: &'a str,
}

/// Reusable template for a cron-backed paracrine heartbeat payload.
///
/// The hotel cron ticker interpolates `{job_id}`, `{timestamp}`,
/// `{iso_timestamp}`, `{node_id}`, and `{target_role}` before dispatch. Keeping
/// the template here gives role authors one canonical registration shape instead
/// of hand-rolled JSON per heartbeat.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ParacrineHeartbeatTemplate {
    /// Typed signal category, for example `open_loop_staleness`.
    pub signal_type: String,
    /// Life Graph scope, for example `personal`, `project`, or `work`.
    pub scope: String,
    /// Subscriber role-type that should receive the signal.
    pub target_role_type: String,
    /// Life Graph node ids or external refs the signal concerns.
    #[serde(default)]
    pub subject_refs: Vec<String>,
    /// Human cadence label used by stewardship policy.
    pub cadence: String,
    /// Signal priority: `low`, `medium`, or `high`.
    pub priority: String,
    /// Optional ISO 8601 expiry template or timestamp.
    #[serde(default)]
    pub expires_at: Option<String>,
    /// Human-readable summary of what caused the heartbeat.
    pub payload_summary: String,
    /// Tags used by SIL/policy matching.
    #[serde(default)]
    pub policy_tags: Vec<String>,
}

impl ParacrineHeartbeatTemplate {
    pub fn attention_steward(
        signal_type: impl Into<String>,
        scope: impl Into<String>,
        cadence: impl Into<String>,
        payload_summary: impl Into<String>,
    ) -> Self {
        Self {
            signal_type: signal_type.into(),
            scope: scope.into(),
            target_role_type: "attention-steward".into(),
            subject_refs: Vec::new(),
            cadence: cadence.into(),
            priority: "medium".into(),
            expires_at: None,
            payload_summary: payload_summary.into(),
            policy_tags: vec!["observe_only".into(), "life_graph".into()],
        }
    }

    pub fn with_subject_refs(
        mut self,
        subject_refs: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.subject_refs = subject_refs.into_iter().map(Into::into).collect();
        self
    }

    pub fn with_policy_tags(
        mut self,
        policy_tags: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.policy_tags = policy_tags.into_iter().map(Into::into).collect();
        self
    }

    pub fn with_priority(mut self, priority: impl Into<String>) -> Self {
        self.priority = priority.into();
        self
    }

    pub fn with_expires_at(mut self, expires_at: impl Into<String>) -> Self {
        self.expires_at = Some(expires_at.into());
        self
    }

    /// Serialize a cron `payload` string that the ticker will normalize into an
    /// `action = "paracrine_signal"` task at fire time.
    pub fn to_cron_payload(&self) -> Result<String> {
        anyhow::ensure!(
            !self.signal_type.trim().is_empty(),
            "signal_type is required"
        );
        anyhow::ensure!(!self.scope.trim().is_empty(), "scope is required");
        anyhow::ensure!(
            !self.target_role_type.trim().is_empty(),
            "target_role_type is required"
        );
        anyhow::ensure!(!self.cadence.trim().is_empty(), "cadence is required");
        anyhow::ensure!(
            !self.payload_summary.trim().is_empty(),
            "payload_summary is required"
        );

        Ok(json!({
            "paracrine_signal": {
                "signal_id": "cron:{job_id}:{timestamp}",
                "signal_type": self.signal_type,
                "scope": self.scope,
                "source_hotel": "{node_id}",
                "source_node": "{node_id}",
                "target_role_type": self.target_role_type,
                "subject_refs": self.subject_refs,
                "cadence": self.cadence,
                "priority": self.priority,
                "observed_at": "{iso_timestamp}",
                "expires_at": self.expires_at,
                "payload_summary": self.payload_summary,
                "policy_tags": self.policy_tags,
            },
            "payload_summary": self.payload_summary,
            "heartbeat": {
                "kind": "cron-backed-paracrine-heartbeat",
                "job_id": "{job_id}",
                "fired_at": "{iso_timestamp}",
                "source_hotel": "{node_id}",
                "target_role": "{target_role}",
            }
        })
        .to_string())
    }
}

impl<'a> CronInterpolationVars<'a> {
    /// Construct vars for a given fire time and job context.
    pub fn new(timestamp_ms: u64, job_id: &'a str, node_id: &'a str, target_role: &'a str) -> Self {
        let iso_timestamp = Utc
            .timestamp_millis_opt(timestamp_ms as i64)
            .single()
            .unwrap_or_else(|| Utc::now())
            .to_rfc3339();
        Self {
            timestamp_ms,
            iso_timestamp,
            job_id,
            node_id,
            target_role,
        }
    }
}

/// Interpolate all `{var}` placeholders in `payload` using the provided vars.
///
/// Supported placeholders:
/// - `{timestamp}` — fire epoch in milliseconds
/// - `{iso_timestamp}` — ISO 8601 date-time string
/// - `{job_id}` — cron job UUID
/// - `{node_id}` — firing hotel node ID
/// - `{target_role}` — destination role inbox
pub fn interpolate_payload(payload: &str, vars: &CronInterpolationVars) -> String {
    payload
        .replace("{timestamp}", &vars.timestamp_ms.to_string())
        .replace("{iso_timestamp}", &vars.iso_timestamp)
        .replace("{job_id}", vars.job_id)
        .replace("{node_id}", vars.node_id)
        .replace("{target_role}", vars.target_role)
}

/// Compute the next fire time (ms since epoch) strictly after `after_ms`.
///
/// `schedule_str` must be a valid cron expression (7-field with seconds).
pub fn next_fire_after(schedule_str: &str, after_ms: u64) -> Result<u64> {
    let schedule = Schedule::from_str(schedule_str)
        .with_context(|| format!("invalid cron expression: {schedule_str}"))?;
    let after_dt = Utc
        .timestamp_millis_opt(after_ms as i64)
        .single()
        .unwrap_or_else(|| Utc::now());
    let next = schedule
        .after(&after_dt)
        .next()
        .with_context(|| format!("cron expression '{schedule_str}' has no future occurrences"))?;
    Ok(next.timestamp_millis() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paracrine_heartbeat_template_emits_attention_steward_contract() {
        let payload = ParacrineHeartbeatTemplate::attention_steward(
            "open_loop_staleness",
            "personal",
            "daily",
            "Scan stale open loops and defer unless policy says to record.",
        )
        .with_subject_refs(["lifegraph:open_loop"])
        .with_policy_tags(["adhd-support", "re-entry"])
        .to_cron_payload()
        .unwrap();

        let value: serde_json::Value = serde_json::from_str(&payload).unwrap();
        let signal = &value["paracrine_signal"];

        assert_eq!(signal["signal_id"], "cron:{job_id}:{timestamp}");
        assert_eq!(signal["signal_type"], "open_loop_staleness");
        assert_eq!(signal["scope"], "personal");
        assert_eq!(signal["source_hotel"], "{node_id}");
        assert_eq!(signal["source_node"], "{node_id}");
        assert_eq!(signal["target_role_type"], "attention-steward");
        assert_eq!(signal["subject_refs"][0], "lifegraph:open_loop");
        assert_eq!(signal["cadence"], "daily");
        assert_eq!(signal["observed_at"], "{iso_timestamp}");
        assert_eq!(signal["policy_tags"][0], "adhd-support");
        assert_eq!(
            value["heartbeat"]["kind"],
            "cron-backed-paracrine-heartbeat"
        );
    }

    #[test]
    fn paracrine_heartbeat_template_rejects_empty_required_fields() {
        let err = ParacrineHeartbeatTemplate::attention_steward(
            "",
            "personal",
            "daily",
            "Scan stale open loops.",
        )
        .to_cron_payload()
        .unwrap_err();

        assert!(err.to_string().contains("signal_type is required"));
    }
}
