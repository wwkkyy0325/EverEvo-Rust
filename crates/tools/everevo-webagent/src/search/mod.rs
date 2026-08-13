//! Web search engine implementations — multi-backend with fallback.
//!
//! Engines are tried in priority order; results from the first successful
//! response are returned. Each engine normalizes its output to a common format.
pub mod engines;
