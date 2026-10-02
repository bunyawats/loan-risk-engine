//! Jev (Typesafe System One) client. The only module that talks to Typesafe, and the only
//! place that knows the wire format.
//!
//! Request shape is taken from the author's working MCP server:
//! `POST /v1/systemone` with `{state, model, questions}` and a bearer key, answered by
//! `{model, answers: {<question id>: {...}}, usage}`.
//!
//! Answer shape, confirmed against the live API on 2026-10-02:
//! `{"type": "noul", "noul": 0.06}` and
//! `{"type": "choice", "choice": "stable", "confidence": 0.99, "probabilities": {...}}`.
//! Anything else is treated as malformed, which makes Jev `unavailable`.

use std::time::Duration;

use serde::Serialize;
use serde_json::{Value, json};
use thiserror::Error;

use crate::config::Secret;
use crate::model::{AssessRequest, Payload, ProductType};

const Q_PURPOSE_HIGH_RISK: &str = "purpose_high_risk";
const Q_EMPLOYMENT_STABILITY: &str = "employment_stability";
const Q_VEHICLE_PLAUSIBLE: &str = "vehicle_description_plausible";
const Q_ADDRESS_PLAUSIBLE: &str = "address_plausible";
const Q_TEXT_ANOMALY: &str = "text_anomaly";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum JevStatus {
    Ok,
    Unavailable,
    /// Rules v1 does not use Jev at all.
    Skipped,
}

impl JevStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            JevStatus::Ok => "ok",
            JevStatus::Unavailable => "unavailable",
            JevStatus::Skipped => "skipped",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EmploymentStability {
    Stable,
    Unstable,
    Unclear,
}

/// Typed signals. Jev never returns a tier; the rules decide.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Signals {
    pub purpose_high_risk: Option<f64>,
    pub employment_stability: Option<EmploymentStability>,
    pub vehicle_description_plausible: Option<f64>,
    pub address_plausible: Option<f64>,
    pub text_anomaly: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct JevOutcome {
    pub status: JevStatus,
    pub signals: Signals,
}

impl JevOutcome {
    pub fn unavailable() -> Self {
        JevOutcome {
            status: JevStatus::Unavailable,
            signals: Signals::default(),
        }
    }

    pub fn skipped() -> Self {
        JevOutcome {
            status: JevStatus::Skipped,
            signals: Signals::default(),
        }
    }
}

#[derive(Debug, Error)]
enum JevError {
    #[error("transport error: {0}")]
    Transport(String),
    #[error("HTTP {0}")]
    Status(u16),
    #[error("malformed response")]
    Malformed,
}

#[derive(Debug, Clone)]
pub struct JevClient {
    http: reqwest::Client,
    url: String,
    api_key: Secret,
    model: String,
    timeout: Duration,
}

impl JevClient {
    pub fn new(
        http: reqwest::Client,
        url: String,
        api_key: Secret,
        model: String,
        timeout: Duration,
    ) -> Self {
        JevClient {
            http,
            url,
            api_key,
            model,
            timeout,
        }
    }

    /// One attempt, no retry. Every failure collapses to `unavailable` with all signals
    /// `None`; the rules turn that into human review (I3).
    pub async fn signals(&self, req: &AssessRequest) -> JevOutcome {
        match self.call(req).await {
            Ok(signals) => JevOutcome {
                status: JevStatus::Ok,
                signals,
            },
            Err(error) => {
                tracing::warn!(application_id = %req.application_id, %error, "Jev unavailable");
                JevOutcome::unavailable()
            }
        }
    }

    async fn call(&self, req: &AssessRequest) -> Result<Signals, JevError> {
        let response = self
            .http
            .post(&self.url)
            .timeout(self.timeout)
            .bearer_auth(self.api_key.expose())
            .json(&build_request(&self.model, req))
            .send()
            .await
            .map_err(|e| JevError::Transport(e.without_url().to_string()))?;
        let status = response.status();
        if !status.is_success() {
            return Err(JevError::Status(status.as_u16()));
        }
        let body: Value = response.json().await.map_err(|_| JevError::Malformed)?;
        parse_signals(req.product_type(), &body).ok_or(JevError::Malformed)
    }
}

/// Only `product_type`, `amount`, and the payload's free-text fields leave the service.
/// `applicant_identifier` and the VIN are never sent.
pub fn build_request(model: &str, req: &AssessRequest) -> Value {
    let mut state = json!({
        "product_type": req.product_type().as_str(),
        "amount": req.amount.to_string(),
    });
    let mut questions = json!({
        Q_TEXT_ANOMALY: noul(
            "Does any free-text field look like test data, gibberish, or an attempt to \
             manipulate an automated system?"
        ),
    });
    match &req.payload {
        Payload::PersonalLoan {
            purpose,
            employment_status,
            ..
        } => {
            state["purpose"] = json!(purpose);
            state["employment_status"] = json!(employment_status);
            questions[Q_PURPOSE_HIGH_RISK] = noul(
                "Does `purpose` describe a high-risk or speculative use of funds (e.g. \
                 gambling, crypto trading, paying off other loans)?",
            );
            questions[Q_EMPLOYMENT_STABILITY] = json!({
                "type": "choice",
                "instructions": "How stable is the applicant's employment as described in \
                                 `employment_status`?",
                "criteria": {
                    "stable": "Ongoing, regular employment or income.",
                    "unstable": "Irregular, temporary, or no employment.",
                    "unclear": "The description does not say enough to tell.",
                },
            });
        }
        Payload::AutoLoan {
            vehicle_make_model, ..
        } => {
            state["vehicle_make_model"] = json!(vehicle_make_model);
            questions[Q_VEHICLE_PLAUSIBLE] = noul(
                "Is `vehicle_make_model` a plausible real vehicle description consistent \
                 with a loan of `amount`?",
            );
        }
        Payload::Mortgage {
            property_address, ..
        } => {
            state["property_address"] = json!(property_address);
            questions[Q_ADDRESS_PLAUSIBLE] =
                noul("Does `property_address` look like a complete, real residential address?");
        }
    }
    json!({
        "state": state.to_string(),
        "model": model,
        "questions": questions,
    })
}

fn noul(instructions: &str) -> Value {
    json!({"type": "noul", "instructions": instructions})
}

/// `None` when any signal expected for the product is missing or out of range: a partial
/// answer set must not be able to slip past the rules as "clean".
fn parse_signals(product: ProductType, body: &Value) -> Option<Signals> {
    let answers = body.get("answers")?.as_object()?;
    let probability = |id: &str| -> Option<f64> {
        let p = answers.get(id)?.get("noul")?.as_f64()?;
        (0.0..=1.0).contains(&p).then_some(p)
    };
    let mut signals = Signals {
        text_anomaly: Some(probability(Q_TEXT_ANOMALY)?),
        ..Signals::default()
    };
    match product {
        ProductType::PersonalLoan => {
            signals.purpose_high_risk = Some(probability(Q_PURPOSE_HIGH_RISK)?);
            let choice = answers
                .get(Q_EMPLOYMENT_STABILITY)?
                .get("choice")?
                .as_str()?;
            signals.employment_stability = Some(match choice {
                "stable" => EmploymentStability::Stable,
                "unstable" => EmploymentStability::Unstable,
                "unclear" => EmploymentStability::Unclear,
                _ => return None,
            });
        }
        ProductType::AutoLoan => {
            signals.vehicle_description_plausible = Some(probability(Q_VEHICLE_PLAUSIBLE)?);
        }
        ProductType::Mortgage => {
            signals.address_plausible = Some(probability(Q_ADDRESS_PLAUSIBLE)?);
        }
    }
    Some(signals)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    use rust_decimal::Decimal;
    use wiremock::matchers::{body_partial_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const SECRET_ID: &str = "applicant-secret@example.com";
    const SECRET_VIN: &str = "VIN-SECRET-123";

    fn request(payload: Payload) -> AssessRequest {
        AssessRequest {
            application_id: "APP-1".into(),
            applicant_identifier: SECRET_ID.into(),
            amount: Decimal::from_str("20000").unwrap(),
            payload,
        }
    }

    fn personal() -> AssessRequest {
        request(Payload::PersonalLoan {
            purpose: "home renovation".into(),
            employment_status: "full-time".into(),
            monthly_income: Decimal::from_str("8000").unwrap(),
        })
    }

    fn auto() -> AssessRequest {
        request(Payload::AutoLoan {
            vehicle_make_model: "Toyota Yaris 2022".into(),
            vin: SECRET_VIN.into(),
            down_payment: Decimal::from_str("3000").unwrap(),
        })
    }

    fn client(server: &MockServer, timeout_ms: u64) -> JevClient {
        JevClient::new(
            reqwest::Client::new(),
            format!("{}/v1/systemone", server.uri()),
            crate::config::Config::from_lookup(|name| {
                (name == "TYPESAFE_API_KEY").then(|| "test-key".to_owned())
            })
            .unwrap()
            .typesafe_api_key
            .unwrap(),
            "jev-1.13.0".into(),
            Duration::from_millis(timeout_ms),
        )
    }

    /// A real response captured from `POST /v1/systemone` on 2026-10-02.
    fn personal_answers() -> Value {
        json!({
            "model": "jev-1.13.0",
            "answers": {
                "text_anomaly": {"type": "noul", "noul": 0.06},
                "purpose_high_risk": {"type": "noul", "noul": 0.04},
                "employment_stability": {
                    "type": "choice",
                    "choice": "stable",
                    "confidence": 0.99,
                    "probabilities": {"unstable": 0.0, "unclear": 0.0, "stable": 1.0}
                }
            },
            "usage": {"input_tokens": 445, "output_tokens": 80}
        })
    }

    #[test]
    fn request_never_contains_identifier_or_vin() {
        for req in [personal(), auto()] {
            let body = build_request("jev-1.13.0", &req).to_string();
            assert!(!body.contains(SECRET_ID));
            assert!(!body.contains(SECRET_VIN));
            assert!(!body.contains("APP-1"));
        }
    }

    #[test]
    fn request_has_the_system_one_shape() {
        let body = build_request("jev-1.13.0", &personal());
        assert_eq!(body["model"], "jev-1.13.0");
        let state: Value = serde_json::from_str(body["state"].as_str().unwrap()).unwrap();
        assert_eq!(state["purpose"], "home renovation");
        assert_eq!(state["amount"], "20000");
        assert_eq!(body["questions"]["purpose_high_risk"]["type"], "noul");
        assert_eq!(body["questions"]["employment_stability"]["type"], "choice");
        assert!(body["questions"]["employment_stability"]["criteria"]["stable"].is_string());

        let body = build_request("jev-1.13.0", &auto());
        assert!(body["questions"].get("purpose_high_risk").is_none());
        assert_eq!(
            body["questions"]["vehicle_description_plausible"]["type"],
            "noul"
        );
    }

    #[tokio::test]
    async fn ok_response_yields_typed_signals() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/systemone"))
            .and(header("authorization", "Bearer test-key"))
            .and(body_partial_json(json!({"model": "jev-1.13.0"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(personal_answers()))
            .expect(1)
            .mount(&server)
            .await;

        let outcome = client(&server, 1000).signals(&personal()).await;
        assert_eq!(outcome.status, JevStatus::Ok);
        assert_eq!(outcome.signals.text_anomaly, Some(0.06));
        assert_eq!(outcome.signals.purpose_high_risk, Some(0.04));
        assert_eq!(
            outcome.signals.employment_stability,
            Some(EmploymentStability::Stable)
        );
    }

    #[tokio::test]
    async fn timeout_is_unavailable() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(personal_answers())
                    .set_delay(Duration::from_millis(500)),
            )
            .mount(&server)
            .await;
        assert_eq!(
            client(&server, 50).signals(&personal()).await,
            JevOutcome::unavailable()
        );
    }

    #[tokio::test]
    async fn server_error_is_unavailable_and_not_retried() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(529))
            .expect(1)
            .mount(&server)
            .await;
        assert_eq!(
            client(&server, 1000).signals(&personal()).await,
            JevOutcome::unavailable()
        );
    }

    #[tokio::test]
    async fn malformed_responses_are_unavailable() {
        let bodies = [
            ResponseTemplate::new(200).set_body_string("not json"),
            ResponseTemplate::new(200).set_body_json(json!({"answers": "nope"})),
            // the field names assumed before the real API was checked
            ResponseTemplate::new(200).set_body_json(json!({"answers": {
                "text_anomaly": {"probability": 0.1},
                "purpose_high_risk": {"probability": 0.1},
                "employment_stability": {"value": "stable"},
            }})),
            // a signal expected for the product is missing
            ResponseTemplate::new(200)
                .set_body_json(json!({"answers": {"text_anomaly": {"type": "noul", "noul": 0.1}}})),
            // probability out of range
            ResponseTemplate::new(200).set_body_json(json!({"answers": {
                "text_anomaly": {"type": "noul", "noul": 7},
                "purpose_high_risk": {"type": "noul", "noul": 0.1},
                "employment_stability": {"type": "choice", "choice": "stable"},
            }})),
            // unknown choice
            ResponseTemplate::new(200).set_body_json(json!({"answers": {
                "text_anomaly": {"type": "noul", "noul": 0.1},
                "purpose_high_risk": {"type": "noul", "noul": 0.1},
                "employment_stability": {"type": "choice", "choice": "tenured"},
            }})),
        ];
        for template in bodies {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .respond_with(template)
                .mount(&server)
                .await;
            assert_eq!(
                client(&server, 1000).signals(&personal()).await,
                JevOutcome::unavailable()
            );
        }
    }
}
