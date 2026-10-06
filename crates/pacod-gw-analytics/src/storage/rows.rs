//! Row shapes that differ from the contract structs.
//!
//! A `UsageEvent` is written as it is: its JSON is already one flat object with one key per
//! column. An `AuditRecord` is not: its four message parts and three JSON values are stored as
//! JSON text, so it is converted to an [`AuditRow`] on the way in and back on the way out.

use chrono::{DateTime, Utc};
use pacod_gw_events::AuditRecord;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const USAGE_TABLE: &str = "usage_events";
pub const AUDIT_TABLE: &str = "message_audit";

/// One row of `message_audit`.
#[derive(Debug, Serialize, Deserialize)]
pub struct AuditRow {
    pub audit_id: String,
    pub event_id: Uuid,
    pub timestamp: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub organisation_id: String,
    pub workspace_id: String,
    pub key_id: String,
    pub end_user: String,
    pub session_id: String,
    pub trace_id: String,
    pub agent_id: String,
    pub user_id: String,
    pub task: String,
    pub complexity: String,
    pub template: String,
    pub model: String,
    pub mode: String,
    pub client_request: String,
    pub provider_requests: String,
    pub provider_responses: String,
    pub client_response: String,
    pub resolution: String,
    pub timeline: String,
    pub outcome: String,
    pub truncated: bool,
}

impl AuditRow {
    pub fn from_record(record: AuditRecord) -> serde_json::Result<Self> {
        Ok(Self {
            client_request: serde_json::to_string(&record.client_request)?,
            provider_requests: serde_json::to_string(&record.provider_requests)?,
            provider_responses: serde_json::to_string(&record.provider_responses)?,
            client_response: serde_json::to_string(&record.client_response)?,
            resolution: serde_json::to_string(&record.resolution)?,
            timeline: serde_json::to_string(&record.timeline)?,
            outcome: serde_json::to_string(&record.outcome)?,
            audit_id: record.audit_id,
            event_id: record.event_id,
            timestamp: record.timestamp,
            expires_at: record.expires_at,
            organisation_id: record.organisation_id,
            workspace_id: record.workspace_id,
            key_id: record.key_id,
            end_user: record.end_user,
            session_id: record.session_id,
            trace_id: record.trace_id,
            agent_id: record.agent_id,
            user_id: record.user_id,
            task: record.task,
            complexity: record.complexity,
            template: record.template,
            model: record.model,
            mode: record.mode,
            truncated: record.truncated,
        })
    }

    pub fn into_record(self) -> serde_json::Result<AuditRecord> {
        Ok(AuditRecord {
            client_request: serde_json::from_str(&self.client_request)?,
            provider_requests: serde_json::from_str(&self.provider_requests)?,
            provider_responses: serde_json::from_str(&self.provider_responses)?,
            client_response: serde_json::from_str(&self.client_response)?,
            resolution: serde_json::from_str(&self.resolution)?,
            timeline: serde_json::from_str(&self.timeline)?,
            outcome: serde_json::from_str(&self.outcome)?,
            audit_id: self.audit_id,
            event_id: self.event_id,
            timestamp: self.timestamp,
            expires_at: self.expires_at,
            organisation_id: self.organisation_id,
            workspace_id: self.workspace_id,
            key_id: self.key_id,
            end_user: self.end_user,
            session_id: self.session_id,
            trace_id: self.trace_id,
            agent_id: self.agent_id,
            user_id: self.user_id,
            task: self.task,
            complexity: self.complexity,
            template: self.template,
            model: self.model,
            mode: self.mode,
            truncated: self.truncated,
        })
    }
}

#[cfg(test)]
mod tests {
    use pacod_gw_events::AuditPart;
    use serde_json::json;

    use super::*;

    #[test]
    fn a_record_survives_the_row_form() {
        let part = AuditPart {
            method: "POST".into(),
            url: "/v1/messages?beta=true".into(),
            headers: vec![("content-type".into(), "application/json".into())],
            body: "{\"model\":\"@planner\"}".into(),
            body_encoding: "utf8".into(),
            body_bytes: 20,
            chunks: vec![(1200, 20)],
            ..AuditPart::default()
        };
        let record = AuditRecord {
            audit_id: "aud_1".into(),
            event_id: Uuid::from_u128(7),
            timestamp: Utc::now(),
            expires_at: Utc::now(),
            organisation_id: "org".into(),
            workspace_id: "ws".into(),
            key_id: "key".into(),
            end_user: "alice".into(),
            session_id: "s".into(),
            trace_id: "t".into(),
            agent_id: "planner-agent".into(),
            user_id: "u-7".into(),
            task: "plan".into(),
            complexity: "high".into(),
            template: "planner".into(),
            model: "claude".into(),
            mode: "managed".into(),
            client_request: part.clone(),
            provider_requests: vec![part.clone(), part.clone()],
            provider_responses: vec![AuditPart { status: 200, ..AuditPart::default() }],
            client_response: AuditPart::default(),
            resolution: json!({"template": "planner", "version": 3}),
            timeline: json!([1, 2, 3]),
            outcome: serde_json::Value::Null,
            truncated: true,
        };

        let row = AuditRow::from_record(record.clone()).expect("row");
        assert!(row.client_request.starts_with('{'));
        assert!(row.provider_requests.starts_with('['));
        assert_eq!(row.outcome, "null");
        assert_eq!(row.into_record().expect("record"), record);
    }
}
