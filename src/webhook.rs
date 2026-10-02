//! The `/decisions` webhook client. The only module that talks to KrakenD.

use std::time::Duration;

use serde::Serialize;
use thiserror::Error;

use crate::model::RiskTier;

/// Kept short: the POC adapter gives the whole `/assess` call 5 seconds.
pub const WEBHOOK_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Serialize)]
struct DecisionMessage<'a> {
    application_id: &'a str,
    risk_tier: RiskTier,
}

#[derive(Debug, Error)]
pub enum WebhookError {
    #[error("/decisions returned HTTP {0}")]
    Status(u16),
    #[error("/decisions transport error: {0}")]
    Transport(String),
}

/// Returns the HTTP status on a 2xx response.
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
        .map_err(|e| WebhookError::Transport(e.without_url().to_string()))?;
    let status = response.status();
    if status.is_success() {
        Ok(status.as_u16())
    } else {
        Err(WebhookError::Status(status.as_u16()))
    }
}
