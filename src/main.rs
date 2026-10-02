use std::net::SocketAddr;

use loan_risk_engine::config::{Config, RulesVersion};
use loan_risk_engine::rules::Rules;
use loan_risk_engine::{AppState, build_router};
use tracing_subscriber::EnvFilter;

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
    match (config.rules_version, config.jev_enabled) {
        (RulesVersion::V2, false) => {
            tracing::warn!("rules v2 with JEV_ENABLED=false: nothing is auto-approved")
        }
        (RulesVersion::V1, true) => tracing::warn!("JEV_ENABLED=true is ignored by rules v1"),
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
