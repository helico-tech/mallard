//! Sentry wire protocol support.
//!
//! This crate is pure (no I/O). It turns bytes sent by Sentry SDKs into
//! normalized events, computes issue grouping and applies JavaScript
//! sourcemaps. The server crate wires it to HTTP and storage.

pub mod auth;
pub mod dsn;
pub mod envelope;
pub mod event;
pub mod grouping;
pub mod store;
pub mod symbolicate;
pub mod useragent;

pub use event::{NormalizeContext, ProcessedEvent, normalize_event};
