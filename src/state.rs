use std::sync::Arc;

use sqlx::SqlitePool;

use crate::{config::Config, hermes::HermesClient, stream::StreamRelay};

/// Shared state handed to every handler.
#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub hermes: HermesClient,
    /// The sidecar's own database, opened with [`crate::db::open`].
    pub db: SqlitePool,
    /// Turns started through `POST /sessions/{id}/chat/stream` that are still running.
    pub streams: StreamRelay,
}

impl AppState {
    pub fn new(config: Config, db: SqlitePool) -> Self {
        let hermes = HermesClient::new(&config);
        Self {
            config: Arc::new(config),
            hermes,
            db,
            streams: StreamRelay::default(),
        }
    }
}
