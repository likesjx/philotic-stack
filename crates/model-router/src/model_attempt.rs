//! Bounded in-memory observations and rate-limited structured logging.
//! No disk store, prompt/reply/message fields, metrics labels or remote exporter.
use ansible_mesh_core::model_attempt::{AttemptRoute, fingerprint};
use philotic_client::TaskErrorPayload;
use serde::Serialize;
use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

const MAX_RECORDS: usize = 128;
const MAX_EVENTS_PER_MINUTE: usize = 64;
const RETENTION_SECS: u64 = 900;

#[derive(Clone, Debug, Serialize)]
pub struct AttemptRecord {
    pub correlation_sha256: String,
    pub requested_model_sha256: String,
    pub resolved_model_sha256: Option<String>,
    pub attempt_id: String,
    pub provider: &'static str,
    pub target_role: String,
    pub route: AttemptRoute,
    pub attempt: u8,
    pub suppressed_before: u64,
    pub outcome: &'static str,
    pub http_status: Option<u16>,
    pub error_category: &'static str,
    pub retryable: bool,
    pub final_for_attempt: bool,
}

fn provider_label(provider: &str) -> (&'static str, &'static str) {
    match provider {
        "ollama" => ("ollama", "model.ollama"),
        "openrouter" => ("openrouter", "model.openrouter"),
        "gemini" => ("gemini", "model"),
        "openai" => ("openai", "model.openai"),
        "anthropic" => ("anthropic", "model.anthropic"),
        _ => ("other", "other"),
    }
}

pub fn category(error: &TaskErrorPayload) -> &'static str {
    match error.sub_kind.as_deref() {
        Some("model_unavailable") => "model_unavailable",
        Some("unsupported_capability") => "unsupported_capability",
        Some("invalid_request") => "invalid_request",
        Some("provider_auth") => "provider_auth",
        Some("rate_limit") => "rate_limit",
        Some("network_error") => "network_error",
        Some("streaming_timeout") => "timeout",
        Some("content_policy_block") => "content_policy_block",
        Some("provider_error") => "provider_error",
        _ => "unclassified",
    }
}

struct Buffer {
    records: VecDeque<(Instant, AttemptRecord)>,
    window: Instant,
    emitted: usize,
    suppressed: u64,
}

impl Buffer {
    fn new(now: Instant) -> Self {
        Self {
            records: VecDeque::new(),
            window: now,
            emitted: 0,
            suppressed: 0,
        }
    }

    fn publish(&mut self, now: Instant, mut record: AttemptRecord) -> Option<AttemptRecord> {
        while self
            .records
            .front()
            .is_some_and(|(at, _)| now.duration_since(*at).as_secs() >= RETENTION_SECS)
        {
            self.records.pop_front();
        }
        if now.duration_since(self.window).as_secs() >= 60 {
            self.window = now;
            self.emitted = 0;
        }
        if self.emitted >= MAX_EVENTS_PER_MINUTE {
            self.suppressed = self.suppressed.saturating_add(1);
            return None;
        }
        self.emitted += 1;
        if self.records.len() == MAX_RECORDS {
            self.records.pop_front();
        }
        record.suppressed_before = self.suppressed;
        self.suppressed = 0;
        self.records.push_back((now, record.clone()));
        Some(record)
    }
}

static BUFFER: OnceLock<Mutex<Buffer>> = OnceLock::new();

fn emit(record: AttemptRecord) {
    let now = Instant::now();
    let buffer = BUFFER.get_or_init(|| Mutex::new(Buffer::new(now)));
    // Telemetry must never poison or block routing on a failed lock/serialization.
    if let Ok(mut buffer) = buffer.try_lock()
        && let Some(record) = buffer.publish(now, record)
        && let Ok(json) = serde_json::to_string(&record)
    {
        tracing::info!(model_attempt = %json, "model attempt observation");
    }
}

pub struct Attempt {
    record: AttemptRecord,
    completed: bool,
}

impl Attempt {
    pub fn start(
        correlation: &str,
        model: &str,
        provider: &str,
        route: AttemptRoute,
        attempt: u8,
    ) -> Self {
        let (provider, provider_role) = provider_label(provider);
        let route = route.projected();
        let target_role = if route.target_role == "unknown" {
            provider_role.into()
        } else {
            route.target_role.clone()
        };
        let record = AttemptRecord {
            correlation_sha256: fingerprint("model-attempt-turn", correlation),
            requested_model_sha256: fingerprint("model-attempt-model", model),
            resolved_model_sha256: None,
            attempt_id: ulid::Ulid::new().to_string(),
            provider,
            target_role,
            route,
            attempt,
            suppressed_before: 0,
            outcome: "started",
            http_status: None,
            error_category: "none",
            retryable: false,
            final_for_attempt: false,
        };
        emit(record.clone());
        Self {
            record,
            completed: false,
        }
    }

    pub fn finish(mut self, error: Option<&TaskErrorPayload>, resolved_model: Option<&str>) {
        self.completed = true;
        self.record.final_for_attempt = true;
        self.record.resolved_model_sha256 =
            resolved_model.map(|model| fingerprint("model-attempt-model", model));
        self.record.outcome = if error.is_some() {
            "failure"
        } else {
            "success"
        };
        if let Some(error) = error {
            self.record.http_status = error.status.filter(|status| (100..=599).contains(status));
            self.record.error_category = category(error);
            self.record.retryable = error.retryable.unwrap_or(false);
        }
        emit(self.record.clone());
    }
}

impl Drop for Attempt {
    fn drop(&mut self) {
        if !self.completed {
            self.record.outcome = "cancelled_or_timeout";
            self.record.final_for_attempt = true;
            emit(self.record.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malicious_provider_errors_are_projected_without_messages_or_labels() {
        let secret = "private prompt Bearer secret\nprovider reply";
        let error = TaskErrorPayload::provider_failure("router", None, Some(secret), secret);
        assert_eq!(category(&error), "unclassified");
        let attempt = Attempt::start(
            secret,
            secret,
            secret,
            AttemptRoute {
                phase: secret.into(),
                reason: secret.into(),
                tier: Some(255),
                ..Default::default()
            },
            1,
        );
        let json = serde_json::to_string(&attempt.record).unwrap();
        assert!(!json.contains(secret));
        assert!(!json.contains("message"));
        assert!(json.len() < 1024);
    }

    #[test]
    fn retention_and_emission_cardinality_are_bounded() {
        let now = Instant::now();
        let mut buffer = Buffer::new(now);
        let record = Attempt::start(
            "synthetic",
            "synthetic",
            "ollama",
            AttemptRoute::default(),
            1,
        )
        .record
        .clone();
        for _ in 0..1000 {
            let _ = buffer.publish(now, record.clone());
        }
        assert_eq!(buffer.records.len(), MAX_EVENTS_PER_MINUTE);
        for minute in 1..10 {
            let at = now + std::time::Duration::from_secs(minute * 60);
            for _ in 0..1000 {
                let _ = buffer.publish(at, record.clone());
            }
        }
        assert_eq!(buffer.records.len(), MAX_RECORDS);
        let _ = buffer.publish(now + std::time::Duration::from_secs(2000), record);
        assert_eq!(buffer.records.len(), 1);
    }
}
