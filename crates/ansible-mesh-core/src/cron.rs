//! Cron job types and schedule computation for the hotel cron subsystem.

use anyhow::{Context, Result};
use chrono::{TimeZone, Utc};
use cron::Schedule;
use serde::{Deserialize, Serialize};
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
