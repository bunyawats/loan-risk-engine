//! `/assess` orchestration: parse → features → jev → rules → webhook → audit log.

use std::time::Instant;

use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::StatusCode;
use serde::Serialize;
use serde_json::{Value, json};

use crate::AppState;
use crate::config::RulesVersion;
use crate::features::{self, Features};
use crate::jev::{JevOutcome, JevStatus, Signals};
use crate::model::{
    AssessRequest, Decision, RULE_DEADLINE, RULE_INVALID_INPUT, RULE_RULES_ERROR,
    salvage_application_id,
};
use crate::rules;
use crate::webhook;

pub async fn healthz(State(state): State<AppState>) -> Json<Value> {
    Json(json!({
        "rules_version": state.rules.label(),
        "rules_sha256": state.rules.sha256(),
        "jev_enabled": state.config.jev_enabled,
    }))
}

/// Always answers 202, and only after the decision webhook has been attempted.
pub async fn assess(State(state): State<AppState>, body: Bytes) -> StatusCode {
    let started = Instant::now();

    let (application_id, product_type, assessment) = match AssessRequest::parse(&body) {
        Ok(req) => {
            let assessment = match tokio::time::timeout(
                state.config.assess_deadline,
                decide(&state, &req),
            )
            .await
            {
                Ok(assessment) => assessment,
                Err(_) => {
                    tracing::error!(application_id = %req.application_id, "assessment deadline exceeded");
                    Assessment::fallback(Decision::medium(
                        RULE_DEADLINE,
                        "assessment deadline exceeded",
                    ))
                }
            };
            let product_type = req.product_type().as_str();
            (req.application_id, Some(product_type), assessment)
        }
        Err(error) => {
            let Some(application_id) = salvage_application_id(&body) else {
                tracing::warn!(%error, "invalid /assess body without an application_id; nothing to decide");
                return StatusCode::ACCEPTED;
            };
            tracing::warn!(%application_id, %error, "invalid /assess body");
            let decision = Decision::medium(RULE_INVALID_INPUT, "invalid or unparsable input");
            (application_id, None, Assessment::fallback(decision))
        }
    };

    let webhook_status = match webhook::post_decision(
        &state.http,
        &state.config.krakend_url,
        &application_id,
        assessment.decision.risk_tier,
    )
    .await
    {
        Ok(status) => Some(status),
        Err(error) => {
            tracing::error!(%application_id, %error, "decision webhook failed");
            match error {
                webhook::WebhookError::Status(status) => Some(status),
                webhook::WebhookError::Transport(_) => None,
            }
        }
    };

    AuditRecord {
        application_id: &application_id,
        product_type,
        decision: &assessment.decision,
        rules_version: state.rules.label(),
        rules_sha256: state.rules.sha256(),
        jev_status: assessment.jev.status,
        jev_model: &state.config.jev_model,
        signals: &assessment.jev.signals,
        features: assessment.features.as_ref(),
        latency_ms: LatencyMs {
            jev: assessment.jev_ms,
            rules: assessment.rules_ms,
            total: started.elapsed().as_millis(),
        },
        webhook_status,
    }
    .emit();

    StatusCode::ACCEPTED
}

struct Assessment {
    decision: Decision,
    features: Option<Features>,
    jev: JevOutcome,
    jev_ms: u128,
    rules_ms: u128,
}

impl Assessment {
    /// A decision made without running features, Jev, or the rules.
    fn fallback(decision: Decision) -> Self {
        Assessment {
            decision,
            features: None,
            jev: JevOutcome::skipped(),
            jev_ms: 0,
            rules_ms: 0,
        }
    }
}

async fn decide(state: &AppState, req: &AssessRequest) -> Assessment {
    if !state.config.simulated_delay.is_zero() {
        tokio::time::sleep(state.config.simulated_delay).await;
    }
    let features = features::compute(req);

    let jev_started = Instant::now();
    let jev = match (state.rules.version(), &state.jev) {
        (RulesVersion::V1, _) => JevOutcome::skipped(),
        (RulesVersion::V2, Some(client)) => client.signals(req).await,
        (RulesVersion::V2, None) => JevOutcome::unavailable(),
    };
    let jev_ms = jev_started.elapsed().as_millis();

    let rules_started = Instant::now();
    let context = rules::context(req.product_type(), &features, &jev);
    let decision = match state.rules.evaluate(context).await {
        Ok(decision) => decision,
        Err(error) => {
            tracing::error!(application_id = %req.application_id, %error, "rules failed; falling back to MEDIUM");
            Decision::medium(RULE_RULES_ERROR, "rules evaluation failed")
        }
    };

    Assessment {
        decision,
        features: Some(features),
        jev,
        jev_ms,
        rules_ms: rules_started.elapsed().as_millis(),
    }
}

#[derive(Debug, Serialize)]
struct LatencyMs {
    jev: u128,
    rules: u128,
    total: u128,
}

/// One line per assessment. By construction it holds no payload text and no
/// `applicant_identifier`: only ids, the decision, features, and signals.
#[derive(Debug, Serialize)]
struct AuditRecord<'a> {
    application_id: &'a str,
    product_type: Option<&'static str>,
    #[serde(flatten)]
    decision: &'a Decision,
    rules_version: &'a str,
    rules_sha256: &'a str,
    jev_status: JevStatus,
    jev_model: &'a str,
    signals: &'a Signals,
    features: Option<&'a Features>,
    latency_ms: LatencyMs,
    webhook_status: Option<u16>,
}

impl AuditRecord<'_> {
    fn emit(&self) {
        // tracing fields cannot hold nested objects, so the nested parts are JSON strings.
        tracing::info!(
            target: "risk_engine::decision",
            application_id = self.application_id,
            product_type = self.product_type,
            risk_tier = self.decision.risk_tier.as_str(),
            rule_id = %self.decision.rule_id,
            reason = %self.decision.reason,
            rules_version = self.rules_version,
            rules_sha256 = self.rules_sha256,
            jev_status = self.jev_status.as_str(),
            jev_model = self.jev_model,
            signals = %json!(self.signals),
            features = %json!(self.features),
            latency_ms = %json!(self.latency_ms),
            webhook_status = self.webhook_status,
            "decision"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::RiskTier;

    #[test]
    fn audit_record_has_the_documented_fields_and_no_pii() {
        let body = json!({
            "application_id": "APP-1",
            "applicant_identifier": "applicant-secret@example.com",
            "product_type": "auto_loan",
            "amount": "32000",
            "payload": {"vehicle_make_model": "SENTINEL-VEHICLE-TEXT", "vin": "SENTINEL-VIN", "down_payment": "3200"}
        })
        .to_string();
        let req = AssessRequest::parse(body.as_bytes()).unwrap();
        let features = features::compute(&req);
        let decision = Decision {
            risk_tier: RiskTier::Medium,
            rule_id: "M5".into(),
            reason: "vehicle implausible".into(),
        };
        let jev = JevOutcome::unavailable();
        let record = serde_json::to_value(AuditRecord {
            application_id: &req.application_id,
            product_type: Some(req.product_type().as_str()),
            decision: &decision,
            rules_version: "risk_tier@v2",
            rules_sha256: "abc",
            jev_status: jev.status,
            jev_model: "jev-1.13",
            signals: &jev.signals,
            features: Some(&features),
            latency_ms: LatencyMs {
                jev: 1,
                rules: 2,
                total: 3,
            },
            webhook_status: Some(202),
        })
        .unwrap();

        for field in [
            "application_id",
            "product_type",
            "risk_tier",
            "rule_id",
            "reason",
            "rules_version",
            "rules_sha256",
            "jev_status",
            "jev_model",
            "signals",
            "features",
            "latency_ms",
            "webhook_status",
        ] {
            assert!(record.get(field).is_some(), "missing {field}");
        }
        assert_eq!(record["risk_tier"], "MEDIUM");
        assert_eq!(record["jev_status"], "unavailable");

        let text = record.to_string();
        for secret in [
            "applicant-secret@example.com",
            "SENTINEL-VEHICLE-TEXT",
            "SENTINEL-VIN",
        ] {
            assert!(!text.contains(secret));
        }
    }
}
