use std::sync::Arc;

use sqlx::SqlitePool;

use crate::{config::Config, hermes::HermesClient};

/// Shared state handed to every handler.
#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub hermes: HermesClient,
    /// The sidecar's own database, opened with [`crate::db::open`].
    pub db: SqlitePool,
}

impl AppState {
    pub fn new(config: Config, db: SqlitePool) -> Self {
        let hermes = HermesClient::new(&config);
        Self {
            config: Arc::new(config),
            hermes,
            db,
        }
    }
}
