use std::path::PathBuf;

/// Files owned by the system service. Profiles are shared by every account.
pub struct Paths {
    pub config: PathBuf,
    pub socket: PathBuf,
    pub pid: PathBuf,
}

impl Paths {
    pub fn system() -> Self {
        Self {
            config: PathBuf::from("/var/lib/y720-kbled/config.json"),
            socket: PathBuf::from("/run/y720-kbled/sock"),
            pid: PathBuf::from("/run/y720-kbled/pid"),
        }
    }
}
