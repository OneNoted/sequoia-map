mod app;
mod config;
mod db_migrations;
mod db_sqlx;
mod routes;
mod services;
mod state;

extern crate self as sqlx;
pub use crate::db_sqlx::{PgPool, Postgres, QueryBuilder, postgres, query, query_as, query_scalar};

use sqlx::postgres::PgPoolOptions;
use std::sync::atomic::Ordering;
use tokio::signal;
use tracing_subscriber::EnvFilter;

use crate::state::AppState;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let database_url = match std::env::var("DATABASE_URL") {
        Ok(value) => value,
        Err(_) => {
            tracing::error!("DATABASE_URL is required to run sequoia-server");
            return;
        }
    };
    let db_max_connections = config::db_max_connections();
    tracing::info!(db_max_connections, "Connecting to PostgreSQL...");
    let db = match PgPoolOptions::new()
        .max_connections(db_max_connections)
        .connect(&database_url)
        .await
    {
        Ok(pool) => pool,
        Err(e) => {
            tracing::error!(error = %e, "failed to connect to PostgreSQL");
            return;
        }
    };
    if let Err(e) = db_migrations::run(&db).await {
        tracing::error!(error = %e, "failed to run migrations");
        return;
    }
    tracing::info!("Database connected and migrations applied");

    let state = AppState::new(Some(db));
    if !state.seq_live_handoff_v1 {
        tracing::warn!("seq_live_handoff_v1 feature flag is disabled");
    }

    if let Some(pool) = state.db.as_ref() {
        match sqlx::query_scalar::<_, Option<i64>>("SELECT MAX(stream_seq) FROM territory_events")
            .fetch_one(pool)
            .await
        {
            Ok(Some(seq)) if seq > 0 => {
                state.next_seq.store(seq as u64, Ordering::Relaxed);
                state.next_seq_reserved.store(seq as u64, Ordering::Relaxed);
                tracing::info!("Initialized stream sequence counter from DB at {seq}");
            }
            Ok(_) => {
                tracing::info!("Initialized stream sequence counter at 0");
            }
            Err(e) => {
                tracing::warn!("Failed to initialize stream sequence counter: {e}");
            }
        }
    }

    services::season_scalar_estimator::warm_cache(&state).await;

    // Spawn background services
    tokio::spawn(services::territory_poller::run(state.clone()));
    tokio::spawn(services::guild_evictor::run(state.clone()));
    tokio::spawn(services::extra_data_loader::run(state.clone()));
    tokio::spawn(services::guild_color_loader::run(state.clone()));
    tokio::spawn(services::season_scalar_estimator::run(state.clone()));

    tokio::spawn(services::snapshot_service::run(state.clone()));
    tokio::spawn(services::retention_cleaner::run(state.clone()));

    let app = app::build_app(state);

    let addr = format!("0.0.0.0:{}", config::SERVER_PORT);
    tracing::info!("Sequoia Map server listening on {addr}");

    let listener = match tokio::net::TcpListener::bind(&addr).await {
        Ok(listener) => listener,
        Err(e) => {
            tracing::error!(error = %e, %addr, "failed to bind TCP listener");
            return;
        }
    };
    if let Err(e) = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
    {
        tracing::error!(error = %e, "server failed");
    }

    tracing::info!("Server shut down gracefully");
}

async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(e) = signal::ctrl_c().await {
            tracing::error!(error = %e, "failed to install Ctrl+C handler");
        }
    };

    #[cfg(unix)]
    let terminate = async {
        let mut sigterm = match signal::unix::signal(signal::unix::SignalKind::terminate()) {
            Ok(sigterm) => sigterm,
            Err(e) => {
                tracing::error!(error = %e, "failed to install SIGTERM handler");
                return;
            }
        };
        sigterm.recv().await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }

    tracing::info!("Shutdown signal received");
}
