//! ZEN decision-table loader and evaluator.

use std::path::Path;
use std::sync::Arc;

use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use thiserror::Error;
use zen_engine::model::{DecisionContent, GraphContent};

use crate::config::RulesVersion;
use crate::features::Features;
use crate::jev::JevOutcome;
use crate::model::{Decision, ProductType, RiskTier};

#[derive(Debug, Error)]
pub enum RulesError {
    #[error("cannot read rules file {path}: {source}")]
    Read {
        path: String,
        source: std::io::Error,
    },
    #[error("rules file {path} is not a decision graph: {reason}")]
    Parse { path: String, reason: String },
    #[error("rules evaluation failed: {0}")]
    Evaluate(String),
    #[error("rules returned an unexpected output: {0}")]
    Output(String),
}

/// What the decision table must return. `risk_tier` is the enum, so any other literal is
/// an `Output` error and never reaches the webhook.
#[derive(Deserialize)]
struct RulesOutput {
    risk_tier: RiskTier,
    rule_id: String,
    reason: String,
}

#[derive(Debug, Clone)]
pub struct Rules {
    graph: Arc<GraphContent>,
    version: RulesVersion,
    label: String,
    sha256: String,
}

impl Rules {
    pub fn load(dir: &Path, version: RulesVersion) -> Result<Self, RulesError> {
        let path = dir.join(format!("risk_tier.{}.json", version.as_str()));
        let display = path.display().to_string();
        let bytes = std::fs::read(&path).map_err(|source| RulesError::Read {
            path: display.clone(),
            source,
        })?;
        Self::from_bytes(&bytes, version).map_err(|reason| RulesError::Parse {
            path: display,
            reason,
        })
    }

    fn from_bytes(bytes: &[u8], version: RulesVersion) -> Result<Self, String> {
        let content: DecisionContent = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
        let mut graph = content
            .as_graph()
            .cloned()
            .ok_or_else(|| "expected a decision graph, got a policy".to_owned())?;
        graph.compile();
        Ok(Rules {
            graph: Arc::new(graph),
            version,
            label: format!("risk_tier@{}", version.as_str()),
            sha256: hex::encode(Sha256::digest(bytes)),
        })
    }

    pub fn version(&self) -> RulesVersion {
        self.version
    }

    /// e.g. `risk_tier@v2`
    pub fn label(&self) -> &str {
        &self.label
    }

    pub fn sha256(&self) -> &str {
        &self.sha256
    }

    /// `zen_engine::Decision::evaluate` returns a `!Send` future, so it runs on a blocking
    /// thread with its own current-thread runtime.
    pub async fn evaluate(&self, context: Value) -> Result<Decision, RulesError> {
        let graph = Arc::clone(&self.graph);
        let result = tokio::task::spawn_blocking(move || -> Result<Value, String> {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .map_err(|e| e.to_string())?;
            runtime.block_on(async {
                let response = zen_engine::Decision::from(graph)
                    .evaluate(context.into())
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(response.result.to_value())
            })
        })
        .await
        .map_err(|e| RulesError::Evaluate(e.to_string()))?
        .map_err(RulesError::Evaluate)?;

        let output: RulesOutput =
            serde_json::from_value(result).map_err(|e| RulesError::Output(e.to_string()))?;
        Ok(Decision {
            risk_tier: output.risk_tier,
            rule_id: output.rule_id,
            reason: output.reason,
        })
    }
}

/// The table's input. Ratios and signals that do not apply are `null`.
pub fn context(product: ProductType, features: &Features, jev: &JevOutcome) -> Value {
    let signals = &jev.signals;
    json!({
        "product_type": product.as_str(),
        "amount": number(Some(features.amount)),
        "loan_to_annual_income": number(features.loan_to_annual_income),
        "down_payment_ratio": number(features.down_payment_ratio),
        "ltv": number(features.ltv),
        "jev_status": jev.status.as_str(),
        "purpose_high_risk": signals.purpose_high_risk,
        "employment_stability": signals.employment_stability,
        "vehicle_description_plausible": signals.vehicle_description_plausible,
        "address_plausible": signals.address_plausible,
        "text_anomaly": signals.text_anomaly,
    })
}

fn number(value: Option<Decimal>) -> Value {
    value
        .and_then(|d| d.to_f64())
        .map_or(Value::Null, Value::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(tier_expression: &str) -> Vec<u8> {
        json!({
            "nodes": [
                {"id": "in", "name": "in", "type": "inputNode", "content": {}},
                {"id": "dt", "name": "dt", "type": "decisionTableNode", "content": {
                    "hitPolicy": "first",
                    "inputs": [{"id": "i1", "name": "Amount", "field": "amount"}],
                    "outputs": [
                        {"id": "o1", "name": "t", "field": "risk_tier"},
                        {"id": "o2", "name": "r", "field": "rule_id"},
                        {"id": "o3", "name": "why", "field": "reason"}
                    ],
                    "rules": [{"_id": "r1", "i1": "", "o1": tier_expression, "o2": "\"X1\"", "o3": "\"because\""}]
                }},
                {"id": "out", "name": "out", "type": "outputNode", "content": {}}
            ],
            "edges": [
                {"id": "e1", "sourceId": "in", "targetId": "dt"},
                {"id": "e2", "sourceId": "dt", "targetId": "out"}
            ]
        })
        .to_string()
        .into_bytes()
    }

    #[tokio::test]
    async fn evaluates_a_table_into_a_typed_decision() {
        let rules = Rules::from_bytes(&table("\"HIGH\""), RulesVersion::V1).unwrap();
        let decision = rules.evaluate(json!({"amount": 1})).await.unwrap();
        assert_eq!(decision.risk_tier, RiskTier::High);
        assert_eq!(decision.rule_id, "X1");
        assert_eq!(rules.label(), "risk_tier@v1");
        assert_eq!(rules.sha256().len(), 64);
    }

    #[tokio::test]
    async fn unknown_tier_is_an_error_not_a_string() {
        let rules = Rules::from_bytes(&table("\"VERY_HIGH\""), RulesVersion::V1).unwrap();
        assert!(matches!(
            rules.evaluate(json!({"amount": 1})).await,
            Err(RulesError::Output(_))
        ));
    }

    #[test]
    fn rejects_files_that_are_not_graphs() {
        assert!(Rules::from_bytes(b"not json", RulesVersion::V1).is_err());
        assert!(Rules::from_bytes(br#"{"nodes": 5}"#, RulesVersion::V1).is_err());
    }
}
