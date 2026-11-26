use serde::{Deserialize, Serialize};

use crate::config::Config;

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op")]
pub enum ClientMsg {
    #[serde(rename = "get")]
    Get,
    #[serde(rename = "set")]
    Set { config: Config },
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
