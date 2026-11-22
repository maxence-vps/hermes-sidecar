//! Sidecar API that adds custom endpoints next to a self-hosted Hermes Agent instance.

pub mod auth;
pub mod config;
pub mod docs;
pub mod error;
pub mod hermes;
pub mod media;
pub mod routes;
pub mod state;

pub use config::Config;
pub use routes::router;
pub use state::AppState;
