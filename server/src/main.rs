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
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let database_url = std::env::var("DATABASE_URL")
        .map_err(|_| "DATABASE_URL is required to run sequoia-server")?;
    let db_max_connections = config::db_max_connections();
    tracing::info!(db_max_connections, "Connecting to PostgreSQL...");
    let db = PgPoolOptions::new()
        .max_connections(db_max_connections)
        .connect(&database_url)
        .await?;
    db_migrations::run(&db).await?;
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

    tokio::spawn(services::territory_poller::run(state.clone()));
    tokio::spawn(services::warcontroller_poller::run(state.clone()));
    tokio::spawn(services::guild_evictor::run(state.clone()));
    tokio::spawn(services::extra_data_loader::run(state.clone()));
    tokio::spawn(services::guild_color_loader::run(state.clone()));
    tokio::spawn(services::season_scalar_estimator::run(state.clone()));

    tokio::spawn(services::snapshot_service::run(state.clone()));
    tokio::spawn(services::retention_cleaner::run(state.clone()));

    let app = app::build_app(state);

    let addr = config::server_bind();
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("Sequoia Map server listening on {addr}");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    tracing::info!("Server shut down gracefully");
    Ok(())
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
