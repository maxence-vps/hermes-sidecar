use std::sync::Arc;

use crate::{config::Config, hermes::HermesClient};

/// Shared state handed to every handler.
#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub hermes: HermesClient,
}

impl AppState {
    pub fn new(config: Config) -> Self {
        let hermes = HermesClient::new(&config);
        Self {
            config: Arc::new(config),
            hermes,
        }
    }
}
