use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use crate::config::Config;
use crate::error::Error;
use crate::paths::Paths;
use crate::proto::{ClientMsg, ServerMsg, PROTOCOL};

pub struct Client {
    reader: BufReader<UnixStream>,
}

impl Client {
    pub fn connect() -> Result<Self, Error> {
        Self::connect_path(&Paths::system().socket)
    }

    pub fn connect_path(path: &Path) -> Result<Self, Error> {
        let stream = UnixStream::connect(path)?;
        stream.set_read_timeout(Some(Duration::from_secs(3)))?;
        stream.set_write_timeout(Some(Duration::from_secs(3)))?;
        Ok(Self {
            reader: BufReader::new(stream),
        })
    }

    pub fn get_config(&mut self) -> Result<Config, Error> {
        self.roundtrip(&ClientMsg::Get)?.into_config()
    }

    pub fn set_config(&mut self, config: Config) -> Result<Config, Error> {
        self.roundtrip(&ClientMsg::Set {
            config,
            version: PROTOCOL,
        })?.into_config()
    }

    pub fn stop(&mut self) -> Result<(), Error> {
        match self.roundtrip(&ClientMsg::Stop)? {
            ServerMsg::Stopped => Ok(()),
            ServerMsg::Error { message, .. } => Err(Error::Msg(message)),
            other => Err(Error::Msg(format!("unexpected stop response: {other:?}"))),
        }
    }

    pub fn read(&mut self) -> Result<ServerMsg, Error> {
        self.read_msg()
    }

    pub fn clone_stream(&self) -> Result<UnixStream, Error> {
        Ok(self.reader.get_ref().try_clone()?)
    }

    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> Result<(), Error> {
        self.reader.get_ref().set_read_timeout(timeout)?;
        Ok(())
    }

    fn roundtrip(&mut self, message: &ClientMsg) -> Result<ServerMsg, Error> {
        let line = serde_json::to_string(message)?;
        let stream = self.reader.get_mut();
        writeln!(stream, "{line}")?;
        stream.flush()?;
        self.read_msg()
    }

    fn read_msg(&mut self) -> Result<ServerMsg, Error> {
        let mut line = String::new();
        let read = self.reader.read_line(&mut line)?;
        if read == 0 {
            return Err(Error::Msg(
                "the backlight service closed the connection".into(),
            ));
        }
        Ok(serde_json::from_str(line.trim())?)
    }
}

impl ServerMsg {
    pub fn into_config(self) -> Result<Config, Error> {
        match self {
            ServerMsg::Config { config, .. } => Ok(config),
            ServerMsg::Error { message, config } => {
                let _ = config;
                Err(Error::Msg(message))
            }
            other => Err(Error::Msg(format!(
                "unexpected service response: {other:?}"
            ))),
        }
    }

    #[cfg(test)]
    pub fn config(&self) -> Option<&Config> {
        match self {
            ServerMsg::Config { config, .. } => Some(config),
            ServerMsg::Error { config, .. } => config.as_ref(),
            _ => None,
        }
    }

    #[cfg(test)]
    pub fn is_show(&self) -> bool {
        matches!(self, ServerMsg::Show)
    }
}
