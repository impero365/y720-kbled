use std::collections::HashMap;
use std::fs;
use std::process::Command;

use crate::error::Error;

/// Ask the root helper to open the window for the person at the keyboard.
/// The service account is allowed to run this one command through sudo.
pub fn open_window() {
    let result = Command::new("sudo")
        .args(["-n", "/usr/bin/y720-kbled", "show-ui"])
        .status();
    match result {
        Ok(status) if status.success() => {}
        Ok(status) => eprintln!("could not open the window: sudo exited {status}"),
        Err(error) => eprintln!("could not open the window: {error}"),
    }
}

pub fn show_active_user_window() -> Result<(), Error> {
    if unsafe { libc::geteuid() } != 0 {
        return Err(Error::Msg(
            "show-ui is only used by the backlight service".into(),
        ));
    }
    let session = active_session()?;
    let mut command = Command::new("systemd-run");
    command.args([
        "--quiet",
        "--collect",
        "--user",
        &format!("--machine={}@.host", session.user),
        "--unit=y720-kbled-ui",
    ]);
    for (key, value) in session_environment(&session) {
        command.arg(format!("--setenv={key}={value}"));
    }
    command.args(["/usr/bin/y720-kbled", "--ui"]);
    let output = command.output()?;
    if output.status.success() {
        return Ok(());
    }
    let detail = String::from_utf8_lossy(&output.stderr);
    let detail = detail.trim();
    Err(Error::Msg(format!(
        "could not open the window for {}{}",
        session.user,
        if detail.is_empty() {
            String::new()
        } else {
            format!(": {detail}")
        }
    )))
}

struct Session {
    user: String,
    uid: u32,
    leader: Option<i32>,
}

fn active_session() -> Result<Session, Error> {
    let output = Command::new("loginctl")
        .args(["list-sessions", "--no-legend"])
        .output()
        .map_err(|error| Error::Msg(format!("loginctl is not available: {error}")))?;
    if !output.status.success() {
        return Err(Error::Msg("loginctl list-sessions failed".into()));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    for line in text.lines() {
        let mut fields = line.split_whitespace();
        let Some(id) = fields.next() else { continue };
        if !id.chars().all(|ch| ch.is_ascii_digit()) {
            continue;
        }
        let Ok(uid) = fields.next().unwrap_or("").parse::<u32>() else {
            continue;
        };
        let Some(user) = fields.next() else { continue };
        if !valid_account(user) {
            continue;
        }
        let props = session_properties(id)?;
        if props.get("Active").map(String::as_str) != Some("yes") {
            continue;
        }
        if props.get("Remote").map(String::as_str) == Some("yes") {
            continue;
        }
        let kind = props.get("Type").map(String::as_str).unwrap_or("");
        if kind != "wayland" && kind != "x11" {
            continue;
        }
        let leader = props
            .get("Leader")
            .and_then(|value| value.parse::<i32>().ok())
            .filter(|pid| *pid > 0);
        return Ok(Session {
            user: user.to_string(),
            uid,
            leader,
        });
    }
    Err(Error::Msg("no local graphical session is active".into()))
}

fn session_properties(id: &str) -> Result<HashMap<String, String>, Error> {
    let output = Command::new("loginctl")
        .args([
            "show-session",
            id,
            "-p",
            "Active",
            "-p",
            "Remote",
            "-p",
            "Type",
            "-p",
            "Leader",
        ])
        .output()?;
    if !output.status.success() {
        return Err(Error::Msg(format!("loginctl show-session {id} failed")));
    }
    Ok(parse_properties(&String::from_utf8_lossy(&output.stdout)))
}

fn parse_properties(text: &str) -> HashMap<String, String> {
    text.lines()
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect()
}

fn session_environment(session: &Session) -> Vec<(String, String)> {
    let mut vars = vec![(
        "XDG_RUNTIME_DIR".to_string(),
        format!("/run/user/{}", session.uid),
    )];
    let Some(leader) = session.leader else {
        return vars;
    };
    let Ok(data) = fs::read(format!("/proc/{leader}/environ")) else {
        return vars;
    };
    for item in data.split(|byte| *byte == 0) {
        let Ok(text) = std::str::from_utf8(item) else {
            continue;
        };
        let Some((key, value)) = text.split_once('=') else {
            continue;
        };
        if matches!(
            key,
            "WAYLAND_DISPLAY" | "DISPLAY" | "DBUS_SESSION_BUS_ADDRESS" | "XDG_CURRENT_DESKTOP"
        ) && !value.is_empty()
            && !value.contains('\n')
        {
            vars.push((key.to_string(), value.to_string()));
        }
    }
    vars
}

fn valid_account(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_lowercase() || first == '_') {
        return false;
    }
    chars.all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_' || ch == '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_names_stay_plain() {
        assert!(valid_account("mattia"));
        assert!(valid_account("_user"));
        assert!(!valid_account(""));
        assert!(!valid_account("Mattia"));
        assert!(!valid_account("user.host"));
        assert!(!valid_account("-mattia"));
        assert!(!valid_account("user name"));
    }

    #[test]
    fn properties_split_on_the_first_equals() {
        let props = parse_properties("Active=yes\nType=wayland\nLeader=42\n");
        assert_eq!(props.get("Active").map(String::as_str), Some("yes"));
        assert_eq!(props.get("Leader").map(String::as_str), Some("42"));
    }
}
