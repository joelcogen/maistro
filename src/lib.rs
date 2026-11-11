pub mod board;
pub mod board_guard;
pub mod codex;
pub mod config;
pub mod engine;
pub mod git;
pub mod github;
pub mod model;
pub mod policy;
pub mod process;
pub mod sandbox;
mod source;
pub mod store;
pub mod verification;

#[cfg(test)]
extern crate self as maistro;
#[cfg(test)]
mod acceptance;
#[cfg(test)]
#[path = "../tests/common/mod.rs"]
pub(crate) mod fixtures;
pub mod mcp_guard;
pub mod network;
pub mod scheduler;
#[cfg(test)]
pub(crate) mod test_support;
