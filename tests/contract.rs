//! The HTTP contract with the POC: `/assess` in, `/decisions` out (KrakenD is wiremock).

use std::collections::HashMap;
use std::path::Path;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use loan_risk_engine::config::Config;
use loan_risk_engine::rules::Rules;
use loan_risk_engine::{AppState, build_router};
use serde_json::{Value, json};
use tower::ServiceExt;
use wiremock::matchers::{body_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// The real router, configured from `vars` and the repo's `rules/` folder.
fn app(vars: &[(&str, &str)]) -> axum::Router {
    let rules_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("rules");
    let mut map: HashMap<String, String> = vars
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    map.insert("RULES_DIR".into(), rules_dir.display().to_string());
    let config = Config::from_lookup(|name| map.get(name).cloned()).expect("config");
    let rules = Rules::load(&config.rules_dir, config.rules_version).expect("rules");
    build_router(AppState::new(config, rules))
}

/// Sends `body` to `POST /assess` and returns the status and response body.
async fn post_assess(app: axum::Router, body: String) -> (StatusCode, Vec<u8>) {
    let response = app
        .oneshot(
            Request::post("/assess")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .expect("request"),
        )
        .await
        .expect("response");
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    (status, bytes.to_vec())
}

/// A valid personal-loan `/assess` body for `amount`.
fn personal(amount: &str) -> Value {
    json!({
        "application_id": "APP-1",
        "applicant_identifier": "a@b.c",
        "product_type": "personal_loan",
        "amount": amount,
        "payload": {"purpose": "home renovation", "employment_status": "full-time", "monthly_income": "8000"}
    })
}

/// A KrakenD stand-in that accepts exactly one `/decisions` call with exactly this body.
async fn krakend_expecting(tier: &str) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/decisions"))
        .and(body_json(
            json!({"application_id": "APP-1", "risk_tier": tier}),
        ))
        .respond_with(ResponseTemplate::new(202))
        .expect(1)
        .mount(&server)
        .await;
    server
}

#[tokio::test]
async fn assess_posts_the_exact_decision_body_then_returns_202() {
    for (amount, tier) in [("5000", "LOW"), ("60000.00", "MEDIUM"), ("150000", "HIGH")] {
        let krakend = krakend_expecting(tier).await;
        let (status, body) = post_assess(
            app(&[("KRAKEND_URL", &krakend.uri())]),
            personal(amount).to_string(),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED);
        assert!(body.is_empty());
        krakend.verify().await;
    }
}

#[tokio::test]
async fn webhook_500_still_returns_202() {
    let krakend = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/decisions"))
        .respond_with(ResponseTemplate::new(500))
        .expect(1)
        .mount(&krakend)
        .await;
    let (status, _) = post_assess(
        app(&[("KRAKEND_URL", &krakend.uri())]),
        personal("60000").to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
}

#[tokio::test]
async fn unreachable_webhook_still_returns_202() {
    let (status, _) = post_assess(
        app(&[("KRAKEND_URL", "http://127.0.0.1:1")]),
        personal("60000").to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
}

/// I5
#[tokio::test]
async fn invalid_input_decides_medium() {
    let mut bad_amount = personal("abc");
    let mut unknown_product = personal("5000");
    unknown_product["product_type"] = json!("student_loan");
    let mut missing_payload_field = personal("5000");
    missing_payload_field["payload"] = json!({"purpose": "x"});
    bad_amount["payload"] = json!({});
    // under v1 these would otherwise match `amount < 15000` and be auto-approved
    let negative_amount = personal("-5000");
    let zero_amount = personal("0");

    for body in [
        bad_amount,
        unknown_product,
        missing_payload_field,
        negative_amount,
        zero_amount,
    ] {
        let krakend = krakend_expecting("MEDIUM").await;
        let (status, _) =
            post_assess(app(&[("KRAKEND_URL", &krakend.uri())]), body.to_string()).await;
        assert_eq!(status, StatusCode::ACCEPTED);
        krakend.verify().await;
    }
}

#[tokio::test]
async fn body_without_application_id_returns_202_and_posts_nothing() {
    for body in ["not json at all", r#"{"amount": "5000"}"#] {
        let krakend = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(202))
            .expect(0)
            .mount(&krakend)
            .await;
        let (status, _) =
            post_assess(app(&[("KRAKEND_URL", &krakend.uri())]), body.to_owned()).await;
        assert_eq!(status, StatusCode::ACCEPTED);
    }
}

/// I3
#[tokio::test]
async fn v2_without_jev_never_auto_approves() {
    let krakend = krakend_expecting("MEDIUM").await;
    let (status, _) = post_assess(
        app(&[("KRAKEND_URL", &krakend.uri()), ("RULES_VERSION", "v2")]),
        personal("5000").to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
}

/// A Jev stand-in that answers every request with `response`.
async fn jev_server(response: ResponseTemplate) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/systemone"))
        .respond_with(response)
        .expect(1)
        .mount(&server)
        .await;
    server
}

/// Runs one v2 assessment against `jev` and checks the posted tier.
async fn assess_v2_with_jev(jev: &MockServer, expected_tier: &str) {
    let krakend = krakend_expecting(expected_tier).await;
    let (status, _) = post_assess(
        app(&[
            ("KRAKEND_URL", &krakend.uri()),
            ("RULES_VERSION", "v2"),
            ("JEV_ENABLED", "true"),
            ("TYPESAFE_API_KEY", "test-key"),
            ("JEV_API_URL", &format!("{}/v1/systemone", jev.uri())),
        ]),
        personal("5000").to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
}

#[tokio::test]
async fn v2_with_clean_jev_signals_approves_a_small_loan() {
    let jev = jev_server(ResponseTemplate::new(200).set_body_json(json!({"answers": {
        "text_anomaly": {"type": "noul", "noul": 0.01},
        "purpose_high_risk": {"type": "noul", "noul": 0.05},
        "employment_stability": {"type": "choice", "choice": "stable"},
    }})))
    .await;
    assess_v2_with_jev(&jev, "LOW").await;
}

#[tokio::test]
async fn v2_jev_signal_escalates_to_medium() {
    let jev = jev_server(ResponseTemplate::new(200).set_body_json(json!({"answers": {
        "text_anomaly": {"type": "noul", "noul": 0.01},
        "purpose_high_risk": {"type": "noul", "noul": 0.9},
        "employment_stability": {"type": "choice", "choice": "stable"},
    }})))
    .await;
    assess_v2_with_jev(&jev, "MEDIUM").await;
}

#[tokio::test]
async fn v2_jev_failure_escalates_to_medium() {
    let jev = jev_server(ResponseTemplate::new(500)).await;
    assess_v2_with_jev(&jev, "MEDIUM").await;
}

#[tokio::test]
async fn deadline_exceeded_still_posts_medium() {
    let krakend = krakend_expecting("MEDIUM").await;
    let (status, _) = post_assess(
        app(&[
            ("KRAKEND_URL", &krakend.uri()),
            ("SIMULATED_DELAY_SECONDS", "5"),
            ("ASSESS_DEADLINE_MS", "50"),
        ]),
        personal("5000").to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
}

#[tokio::test]
async fn healthz_reports_rules_and_jev_state() {
    let response = app(&[])
        .oneshot(
            Request::get("/healthz")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(body["rules_version"], "risk_tier@v1");
    assert_eq!(body["rules_sha256"].as_str().map(str::len), Some(64));
    assert_eq!(body["jev_enabled"], false);
}
