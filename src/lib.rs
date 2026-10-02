//! Risk-assessment service for loan-onboarding-poc. See CLAUDE.md for the contract.

pub mod config;
pub mod features;
pub mod handler;
pub mod jev;
pub mod model;
pub mod rules;
pub mod webhook;

use std::sync::Arc;

use axum::Router;
use axum::routing::{get, post};

use crate::config::Config;
use crate::jev::JevClient;
use crate::rules::Rules;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub rules: Arc<Rules>,
    pub http: reqwest::Client,
    /// `None` when `JEV_ENABLED=false`.
    pub jev: Option<Arc<JevClient>>,
}

impl AppState {
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

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/assess", post(handler::assess))
        .route("/healthz", get(handler::healthz))
        .with_state(state)
}
