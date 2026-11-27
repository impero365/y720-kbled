use serde::{Deserialize, Serialize};

use crate::config::Config;

/// Raised when a `Set` from an older client would lose fields. Version 2
/// added Split zones: an older window sends profiles without it, and the
/// service would read them as divided with a blank whole side.
pub const PROTOCOL: u32 = 2;

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op")]
pub enum ClientMsg {
    #[serde(rename = "get")]
    Get,
    #[serde(rename = "set")]
    Set {
        config: Config,
        /// Missing from clients older than version 2.
        #[serde(default)]
        version: u32,
    },
    #[serde(rename = "cycle")]
    Cycle,
    #[serde(rename = "stop")]
    Stop,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ServerMsg {
    #[serde(rename = "config")]
    Config {
        config: Config,
        #[serde(default)]
        notice: String,
    },
    #[serde(rename = "show")]
    Show,
    #[serde(rename = "error")]
    Error {
        message: String,
        #[serde(default)]
        config: Option<Config>,
    },
    #[serde(rename = "stopped")]
    Stopped,
}
