//! Risk-assessment service for loan-onboarding-poc. See CLAUDE.md for the contract.
//!
//! The library holds everything; `main.rs` only reads config, loads the rules, and serves
//! [`build_router`]. Tests build the same router with their own [`AppState`].
//!
//! Module map:
//! - [`config`]: environment variables, parsed once at startup.
//! - [`model`]: request, payload, tier, and decision types (no I/O).
//! - [`features`]: deterministic ratios computed from the request (no I/O).
//! - [`jev`]: the Typesafe Jev client that turns free text into signals.
//! - [`rules`]: the ZEN decision-table loader and evaluator.
//! - [`webhook`]: the `/decisions` client that reports the tier back to KrakenD.
//! - [`handler`]: the Axum handlers that tie the steps together.

pub mod config;
pub mod features;
pub mod handler;
pub mod jev;
pub mod model;
pub mod rules;
pub mod webhook;

use std::error::Error;
use std::fmt;
use std::sync::Arc;

use axum::Router;
use axum::routing::{get, post};

use crate::config::Config;
use crate::jev::JevClient;
use crate::rules::Rules;

/// Shared, read-only state handed to every request. Cloning is cheap: each field is an
/// `Arc` or a handle that is itself reference-counted.
#[derive(Clone)]
pub struct AppState {
    /// Settings parsed from the environment at startup.
    pub config: Arc<Config>,
    /// The active decision table (`RULES_VERSION`), parsed and compiled once.
    pub rules: Arc<Rules>,
    /// Connection pool shared by the Jev client and the webhook. Each call sets its own
    /// timeout, so the client itself has none.
    pub http: reqwest::Client,
    /// `None` when `JEV_ENABLED=false`.
    pub jev: Option<Arc<JevClient>>,
}

impl AppState {
    /// Builds the state, creating a [`JevClient`] only when `JEV_ENABLED=true` and a
    /// `TYPESAFE_API_KEY` is set. (`Config` already refuses the first without the second.)
    pub fn new(config: Config, rules: Rules) -> Self {
        let http = reqwest::Client::new();
        let jev = match (&config.typesafe_api_key, config.jev_enabled) {
            (Some(key), true) => Some(Arc::new(JevClient::new(
                http.clone(),
                config.jev_api_url.clone(),
                key.clone(),
                config.jev_model.clone(),
                config.jev_timeout,
            ))),
            _ => None,
        };
        AppState {
            config: Arc::new(config),
            rules: Arc::new(rules),
            http,
            jev,
        }
    }
}

/// The HTTP surface: `POST /assess` (the contract) and `GET /healthz` (additive).
///
/// # Examples
///
/// ```
/// use axum::body::Body;
/// use axum::http::{Request, StatusCode};
/// use loan_risk_engine::config::Config;
/// use loan_risk_engine::rules::Rules;
/// use loan_risk_engine::{AppState, build_router};
/// use tower::ServiceExt;
///
/// # #[tokio::main]
/// # async fn main() {
/// let config = Config::from_lookup(|_| None).unwrap(); // all defaults: rules v1, Jev off
/// let rules_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("rules");
/// let rules = Rules::load(&rules_dir, config.rules_version).unwrap();
///
/// let response = build_router(AppState::new(config, rules))
///     .oneshot(Request::get("/healthz").body(Body::empty()).unwrap())
///     .await
///     .unwrap();
/// assert_eq!(response.status(), StatusCode::OK);
/// # }
/// ```
pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/assess", post(handler::assess))
        .route("/healthz", get(handler::healthz))
        .with_state(state)
}

/// Displays an error followed by its whole `source()` chain: `outer: cause: root cause`.
///
/// Error types here keep their cause as a `#[source]` and leave it out of their own
/// message, so a plain `%error` in a log line would drop the detail that matters (e.g.
/// "connection refused"). Log `%ErrorChain(&error)` instead. A cause whose message repeats
/// the previous one is skipped, since some libraries print their source as their own message.
pub(crate) struct ErrorChain<'a>(pub(crate) &'a (dyn Error + 'static));

impl fmt::Display for ErrorChain<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut previous = self.0.to_string();
        f.write_str(&previous)?;
        let mut source = self.0.source();
        while let Some(error) = source {
            let message = error.to_string();
            if message != previous {
                write!(f, ": {message}")?;
            }
            previous = message;
            source = error.source();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, thiserror::Error)]
    #[error("{message}")]
    struct Layer {
        message: &'static str,
        #[source]
        source: Option<Box<Layer>>,
    }

    fn layer(message: &'static str, source: Option<Layer>) -> Layer {
        Layer {
            message,
            source: source.map(Box::new),
        }
    }

    #[test]
    fn error_chain_prints_every_cause_once() {
        let error = layer(
            "webhook failed",
            Some(layer(
                "connection refused",
                Some(layer("connection refused", None)),
            )),
        );
        assert_eq!(
            ErrorChain(&error).to_string(),
            "webhook failed: connection refused"
        );
        assert_eq!(ErrorChain(&layer("alone", None)).to_string(), "alone");
    }
}
