//! Rules v1 must reproduce the POC mock's boundary table exactly
//! (loan-onboarding-poc: mock_risk_engine/tests/test_main.py).

use std::path::Path;

use loan_risk_engine::config::RulesVersion;
use loan_risk_engine::features;
use loan_risk_engine::jev::JevOutcome;
use loan_risk_engine::model::{AssessRequest, RiskTier};
use loan_risk_engine::rules::{self, Rules};
use serde_json::json;

/// `(amount, expected tier)` pairs from the mock's own tests.
const MOCK_BOUNDARY_TABLE: [(&str, RiskTier); 8] = [
    ("1", RiskTier::Low),
    ("14999.99", RiskTier::Low),
    ("15000", RiskTier::Medium),
    ("49999.99", RiskTier::Medium),
    ("50000", RiskTier::Medium),
    ("99999.99", RiskTier::Medium),
    ("100000", RiskTier::High),
    ("150000", RiskTier::High),
];

/// One valid request per product type for `amount`, so v1 is shown to ignore the product.
fn requests(amount: &str) -> Vec<AssessRequest> {
    [
        json!({"product_type": "personal_loan", "payload": {"purpose": "p", "employment_status": "e", "monthly_income": "1"}}),
        json!({"product_type": "auto_loan", "payload": {"vehicle_make_model": "m", "vin": "v", "down_payment": "0"}}),
        json!({"product_type": "mortgage", "payload": {"property_address": "a", "appraised_value": "1", "down_payment": "0"}}),
    ]
    .into_iter()
    .map(|mut body| {
        body["application_id"] = json!("APP-1");
        body["applicant_identifier"] = json!("x");
        body["amount"] = json!(amount);
        AssessRequest::parse(body.to_string().as_bytes()).expect("valid request")
    })
    .collect()
}

#[tokio::test]
async fn v1_matches_the_mock_boundary_table_for_every_product() {
    let rules = Rules::load(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("rules"),
        RulesVersion::V1,
    )
    .expect("v1 rules load");

    for (amount, expected) in MOCK_BOUNDARY_TABLE {
        for req in requests(amount) {
            let context = rules::context(
                req.product_type(),
                &features::compute(&req),
                &JevOutcome::skipped(),
            );
            let decision = rules.evaluate(context).await.expect("rules evaluate");
            assert_eq!(
                decision.risk_tier,
                expected,
                "amount {amount}, product {:?}",
                req.product_type()
            );
        }
    }
}
