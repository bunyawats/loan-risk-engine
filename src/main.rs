//! Binary entry point: JSON logging, config, rules, then the Axum server on `PORT`.
//!
//! Every failure here is a startup failure and panics on purpose (fail fast). Nothing on
//! the request path lives in this file.

use std::net::SocketAddr;

use loan_risk_engine::config::Config;
use loan_risk_engine::rules::Rules;
use loan_risk_engine::{AppState, build_router};
use tracing_subscriber::EnvFilter;

/// Starts the service. Logs the active rules version and SHA-256 at startup, and warns
/// when `JEV_ENABLED` and `RULES_VERSION` disagree (v2/v3 without Jev never auto-approves;
/// v1 ignores Jev).
#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .json()
        .flatten_event(true)
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let config = Config::from_env().expect("invalid configuration");
    let rules = Rules::load(&config.rules_dir, config.rules_version).expect("cannot load rules");
    tracing::info!(
        rules_version = rules.label(),
        rules_sha256 = rules.sha256(),
        jev_enabled = config.jev_enabled,
        "starting"
    );
    match (config.rules_version.uses_jev(), config.jev_enabled) {
        (true, false) => {
            tracing::warn!("Jev-based rules with JEV_ENABLED=false: nothing is auto-approved")
        }
        (false, true) => tracing::warn!("JEV_ENABLED=true is ignored by rules v1"),
        _ => {}
    }

    let addr = SocketAddr::from(([0, 0, 0, 0], config.port));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("cannot bind listen port");
    tracing::info!(%addr, "listening");

    axum::serve(listener, build_router(AppState::new(config, rules)))
        .with_graceful_shutdown(shutdown_signal())
        .await
        .expect("server error");
}

/// Resolves on Ctrl-C, or on SIGTERM on Unix (what `docker stop` sends), so in-flight
/// requests can finish before the server exits.
async fn shutdown_signal() {
    let ctrl_c = tokio::signal::ctrl_c();
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("cannot install SIGTERM handler");
        tokio::select! {
            _ = ctrl_c => {}
            _ = terminate.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = ctrl_c.await;
    }
}
