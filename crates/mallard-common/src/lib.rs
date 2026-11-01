//! Shared building blocks of mallard's two sides (Sentry `sdl-*`, OTel
//! `odl-*`) and of `mallard-*`.
//!
//! Always available (light: serde, chrono, humantime):
//! - [`duration`]: `5s`/`7d`/`0` parsing and serde,
//! - [`ByteSize`]: `512MB`/`2GiB` sizes,
//! - [`window`]: query time windows,
//! - [`ApiError`]: the JSON error body of both APIs.
//!
//! Feature `server` (axum, toml, tracing-subscriber, Prometheus):
//! [`config::load`], [`auth::Tokens`], [`http`] helpers, and process set-up
//! ([`init_tracing`], [`shutdown_signal`], [`install_prometheus`]).

pub mod bytes;
pub mod duration;
pub mod window;

pub use bytes::ByteSize;

/// Error body returned with every non-2xx response.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ApiError {
    /// Machine readable error code, e.g. `bad_request`, `not_found`, `timeout`.
    pub error: String,
    /// Human readable message.
    pub message: String,
}

#[cfg(feature = "server")]
pub mod auth;
#[cfg(feature = "server")]
pub mod config;
#[cfg(feature = "server")]
pub mod http;
#[cfg(feature = "server")]
mod runtime;
#[cfg(feature = "server")]
pub use runtime::{LogFormat, init_tracing, install_prometheus, shutdown_signal};
