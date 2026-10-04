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

use crate::ErrorChain;
use crate::config::RulesVersion;
use crate::features::Features;
use crate::jev::JevOutcome;
use crate::model::{Decision, ProductType, RiskTier};

/// Loading errors (`Read`, `Parse`, `NotAGraph`) stop startup. The others happen during
/// evaluation and become a MEDIUM decision with rule `R-RULES-ERROR` (I1).
///
/// Each variant keeps its underlying error as `source()` where it can, so logs can print
/// the whole chain. Messages leave the source out to avoid printing it twice.
#[derive(Debug, Error)]
pub enum RulesError {
    /// The rules file could not be read from disk.
    #[error("cannot read rules file {path}")]
    Read {
        path: String,
        source: std::io::Error,
    },
    /// The file is not valid JSON, or not a valid ZEN document.
    #[error("cannot parse rules file {path}")]
    Parse {
        path: String,
        source: serde_json::Error,
    },
    /// The file is a ZEN "policy" document; only a decision graph can be evaluated.
    #[error("rules file {path} is a policy, not a decision graph")]
    NotAGraph { path: String },
    /// The current-thread runtime for the evaluation could not be built.
    #[error("cannot start the rules runtime")]
    Runtime(#[source] std::io::Error),
    /// The blocking evaluation task panicked or was cancelled.
    #[error("rules evaluation task failed")]
    Task(#[source] tokio::task::JoinError),
    /// The ZEN engine reported an error. Its error type is `!Send` and cannot leave the
    /// blocking thread, so it arrives here as text, cause chain included.
    #[error("rules evaluation failed: {0}")]
    Engine(String),
    /// The table ran but returned something other than `RulesOutput`, such as an
    /// unknown tier literal or a missing field.
    #[error("rules returned an unexpected output")]
    Output(#[source] serde_json::Error),
}

/// What the decision table must return. `risk_tier` is the enum, so any other literal is
/// an `Output` error and never reaches the webhook.
#[derive(Deserialize)]
struct RulesOutput {
    risk_tier: RiskTier,
    rule_id: String,
    reason: String,
}

/// The active decision table, loaded and compiled once at startup.
#[derive(Debug, Clone)]
pub struct Rules {
    /// The compiled ZEN graph, shared with each blocking evaluation.
    graph: Arc<GraphContent>,
    /// Which `RULES_VERSION` this is. Decides whether Jev is consulted.
    version: RulesVersion,
    /// `risk_tier@<version>`, reported as `rules_version` in logs and `/healthz`.
    label: String,
    /// Hex SHA-256 of the file's exact bytes, so every decision can be traced to the table
    /// that made it.
    sha256: String,
}

impl Rules {
    /// Reads `<dir>/risk_tier.<version>.json`, parses it, and compiles it.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::path::Path;
    ///
    /// use loan_risk_engine::config::RulesVersion;
    /// use loan_risk_engine::rules::Rules;
    ///
    /// let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("rules");
    /// let rules = Rules::load(&dir, RulesVersion::V1).unwrap();
    /// assert_eq!(rules.label(), "risk_tier@v1");
    /// assert_eq!(rules.sha256().len(), 64); // hex SHA-256
    ///
    /// assert!(Rules::load(Path::new("/no/such/dir"), RulesVersion::V1).is_err());
    /// ```
    pub fn load(dir: &Path, version: RulesVersion) -> Result<Self, RulesError> {
        let path = dir.join(format!("risk_tier.{}.json", version.as_str()));
        let display = path.display().to_string();
        let bytes = std::fs::read(&path).map_err(|source| RulesError::Read {
            path: display.clone(),
            source,
        })?;
        Self::from_bytes(&bytes, version, &display)
    }

    /// Parses and compiles a table from raw bytes. `path` is only used in error messages.
    /// A ZEN "policy" document is rejected; only a graph works.
    fn from_bytes(bytes: &[u8], version: RulesVersion, path: &str) -> Result<Self, RulesError> {
        let content: DecisionContent =
            serde_json::from_slice(bytes).map_err(|source| RulesError::Parse {
                path: path.to_owned(),
                source,
            })?;
        let mut graph = content
            .as_graph()
            .cloned()
            .ok_or_else(|| RulesError::NotAGraph {
                path: path.to_owned(),
            })?;
        graph.compile();
        Ok(Rules {
            graph: Arc::new(graph),
            version,
            label: format!("risk_tier@{}", version.as_str()),
            sha256: hex::encode(Sha256::digest(bytes)),
        })
    }

    /// The loaded rules version.
    pub fn version(&self) -> RulesVersion {
        self.version
    }

    /// e.g. `risk_tier@v2`
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Hex SHA-256 of the rules file.
    pub fn sha256(&self) -> &str {
        &self.sha256
    }

    /// `zen_engine::Decision::evaluate` returns a `!Send` future, so it runs on a blocking
    /// thread with its own current-thread runtime.
    ///
    /// `context` is the table input, normally built by [`context`]. The table's output is
    /// checked against `RulesOutput`; anything else is a [`RulesError::Output`].
    ///
    /// # Examples
    ///
    /// ```
    /// use std::path::Path;
    ///
    /// use loan_risk_engine::config::RulesVersion;
    /// use loan_risk_engine::features::Features;
    /// use loan_risk_engine::jev::JevOutcome;
    /// use loan_risk_engine::model::{ProductType, RiskTier};
    /// use loan_risk_engine::rules::{Rules, context};
    /// use rust_decimal::Decimal;
    ///
    /// # #[tokio::main]
    /// # async fn main() {
    /// let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("rules");
    /// let rules = Rules::load(&dir, RulesVersion::V1).unwrap();
    /// let features = Features {
    ///     amount: Decimal::new(60_000, 0),
    ///     loan_to_annual_income: None,
    ///     down_payment_ratio: None,
    ///     ltv: None,
    /// };
    /// let decision = rules
    ///     .evaluate(context(ProductType::AutoLoan, &features, &JevOutcome::skipped()))
    ///     .await
    ///     .unwrap();
    /// assert_eq!(decision.risk_tier, RiskTier::Medium); // v1: 15k <= amount < 100k
    /// # }
    /// ```
    pub async fn evaluate(&self, context: Value) -> Result<Decision, RulesError> {
        let graph = Arc::clone(&self.graph);
        let result = tokio::task::spawn_blocking(move || -> Result<Value, RulesError> {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .map_err(RulesError::Runtime)?;
            runtime.block_on(async {
                let response = zen_engine::Decision::from(graph)
                    .evaluate(context.into())
                    .await
                    .map_err(|e| RulesError::Engine(ErrorChain(&*e).to_string()))?;
                Ok(response.result.to_value())
            })
        })
        .await
        .map_err(RulesError::Task)??;

        let output: RulesOutput = serde_json::from_value(result).map_err(RulesError::Output)?;
        Ok(Decision {
            risk_tier: output.risk_tier,
            rule_id: output.rule_id,
            reason: output.reason,
        })
    }
}

/// The table's input. Ratios and signals that do not apply are `null`.
///
/// Field names here are the `field` names the JSON tables match on, so renaming one is a
/// rules change. Decimals become JSON numbers (`f64`) because that is what ZEN compares.
///
/// # Examples
///
/// ```
/// use loan_risk_engine::features::Features;
/// use loan_risk_engine::jev::JevOutcome;
/// use loan_risk_engine::model::ProductType;
/// use loan_risk_engine::rules::context;
/// use rust_decimal::Decimal;
///
/// let features = Features {
///     amount: Decimal::new(90_000, 0),
///     loan_to_annual_income: None,
///     down_payment_ratio: Some(Decimal::new(25, 2)),
///     ltv: Some(Decimal::new(75, 2)),
/// };
/// let input = context(ProductType::Mortgage, &features, &JevOutcome::unavailable());
/// assert_eq!(input["product_type"], "mortgage");
/// assert_eq!(input["ltv"], 0.75);
/// assert_eq!(input["jev_status"], "unavailable");
/// assert!(input["loan_to_annual_income"].is_null());
/// assert!(input["address_plausible"].is_null());
/// ```
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

/// A decimal as a JSON number, or `null` when absent or not representable as `f64`.
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
        let rules = Rules::from_bytes(&table("\"HIGH\""), RulesVersion::V1, "test.json").unwrap();
        let decision = rules.evaluate(json!({"amount": 1})).await.unwrap();
        assert_eq!(decision.risk_tier, RiskTier::High);
        assert_eq!(decision.rule_id, "X1");
        assert_eq!(rules.label(), "risk_tier@v1");
        assert_eq!(rules.sha256().len(), 64);
    }

    #[tokio::test]
    async fn unknown_tier_is_an_error_not_a_string() {
        let rules =
            Rules::from_bytes(&table("\"VERY_HIGH\""), RulesVersion::V1, "test.json").unwrap();
        assert!(matches!(
            rules.evaluate(json!({"amount": 1})).await,
            Err(RulesError::Output(_))
        ));
    }

    #[test]
    fn rejects_files_that_are_not_graphs() {
        assert!(matches!(
            Rules::from_bytes(b"not json", RulesVersion::V1, "test.json"),
            Err(RulesError::Parse { .. })
        ));
        assert!(matches!(
            Rules::from_bytes(br#"{"nodes": 5}"#, RulesVersion::V1, "test.json"),
            Err(RulesError::Parse { .. })
        ));
    }
}
