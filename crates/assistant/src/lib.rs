//! L5: the AI assistant. A chat agent that answers questions about the map and acts on it by
//! running RustGIS commands and geoprocessing tools, through a model of the user's choosing
//! (Anthropic, OpenAI, a ChatGPT account, Gemini, OpenRouter, Ollama or any OpenAI-compatible
//! server).
//!
//! - [`provider`]: the four wire formats, as plain request/response data.
//! - [`chatgpt`]: signing in with ChatGPT, so the user's ChatGPT plan is used instead of a key.
//! - [`tools`]: what the model may call, and which calls need the user's approval.
//! - [`agent`]: the conversation loop, without I/O, so the desktop (a thread), the browser
//!   (`fetch`), the command line and the tests drive the same code.
//! - [`http`]: the blocking transport for the desktop and the command line.
//! - [`mcp`]: the same tools for any Model Context Protocol client (`rustgis-cli mcp`).
//! - [`docs`]: the documentation, built in, for MCP clients to search and read.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod acp;
pub mod agent;
pub mod attach;
#[cfg(not(target_arch = "wasm32"))]
pub mod audit;
pub mod chatgpt;
pub mod compact;
pub mod docs;
pub mod error;
#[cfg(not(target_arch = "wasm32"))]
pub mod filebridge;
pub use error::{Error, Recovery};
pub mod framing;
#[cfg(not(target_arch = "wasm32"))]
pub mod http;
pub mod mcp;
pub mod provider;
#[cfg(not(target_arch = "wasm32"))]
pub mod relay;
pub mod skills;
#[cfg(test)]
mod tests;
pub mod tools;

pub use agent::{Agent, Entry, Step};
pub use provider::{Api, Config, HttpRequest, HttpResponse, Note, PRESETS, Preset};
