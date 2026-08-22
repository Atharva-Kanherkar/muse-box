//! muse-box backend: fat backend, dumb clients.
//!
//! The public API is two endpoints (`GET /state` SSE, `POST /voice`) that
//! speak the versioned [`render::RenderDoc`]. Everything else is
//! implementation detail. See README.md and AGENTS.md for the contract.

pub mod config;
pub mod error;
pub mod idle;
pub mod image;
pub mod lyrics;
pub mod realtime;
pub mod render;
pub mod routes;
pub mod session;
pub mod spotify;
pub mod state;
pub mod taste;
