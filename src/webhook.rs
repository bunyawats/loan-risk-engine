//! The `/decisions` webhook client. The only module that talks to KrakenD.

use std::time::Duration;

use serde::Serialize;
use thiserror::Error;

use crate::model::RiskTier;

/// Kept short: the POC adapter gives the whole `/assess` call 5 seconds.
pub const WEBHOOK_TIMEOUT: Duration = Duration::from_secs(2);

/// The exact `/decisions` body from the contract: `{application_id, risk_tier}`.
#[derive(Debug, Serialize)]
struct DecisionMessage<'a> {
    application_id: &'a str,
    /// Always one of the three enum values, because the POC's Temporal signal crashes on
    /// anything else.
    risk_tier: RiskTier,
}

/// Why the webhook failed. The caller logs it and still answers 202.
#[derive(Debug, Error)]
pub enum WebhookError {
    /// KrakenD answered with a non-2xx status.
    #[error("/decisions returned HTTP {0}")]
    Status(u16),
    /// Connection failure or timeout. The URL is stripped from the error.
    #[error("/decisions transport error")]
    Transport(#[source] reqwest::Error),
}

/// Posts the decision to `{krakend_url}/decisions` with a [`WEBHOOK_TIMEOUT`] timeout.
/// One attempt, no retry. Returns the HTTP status on a 2xx response.
///
/// # Examples
///
/// ```
/// use loan_risk_engine::model::RiskTier;
/// use loan_risk_engine::webhook::{WebhookError, post_decision};
/// use serde_json::json;
/// use wiremock::matchers::{body_json, method, path};
/// use wiremock::{Mock, MockServer, ResponseTemplate};
///
/// # #[tokio::main]
/// # async fn main() {
/// // A KrakenD stand-in that accepts exactly this body once.
/// let krakend = MockServer::start().await;
/// Mock::given(method("POST"))
///     .and(path("/decisions"))
///     .and(body_json(json!({"application_id": "APP-1", "risk_tier": "LOW"})))
///     .respond_with(ResponseTemplate::new(202))
///     .expect(1)
///     .mount(&krakend)
///     .await;
///
/// let http = reqwest::Client::new();
/// let status = post_decision(&http, &krakend.uri(), "APP-1", RiskTier::Low).await;
/// assert_eq!(status.unwrap(), 202);
///
/// // Any other body gets wiremock's 404, which comes back as an error.
/// let err = post_decision(&http, &krakend.uri(), "APP-2", RiskTier::Low).await;
/// assert!(matches!(err, Err(WebhookError::Status(404))));
/// # }
/// ```
pub async fn post_decision(
    http: &reqwest::Client,
    krakend_url: &str,
    application_id: &str,
    risk_tier: RiskTier,
) -> Result<u16, WebhookError> {
    let response = http
        .post(format!("{krakend_url}/decisions"))
        .timeout(WEBHOOK_TIMEOUT)
        .json(&DecisionMessage {
            application_id,
            risk_tier,
        })
        .send()
        .await
        .map_err(|e| WebhookError::Transport(e.without_url()))?;
    let status = response.status();
    if status.is_success() {
        Ok(status.as_u16())
    } else {
        Err(WebhookError::Status(status.as_u16()))
    }
}
