use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::config::{Config, Profile};
use crate::error::Error;
use crate::hw::Hotkey;
use crate::proto::{ClientMsg, ServerMsg};

enum Work {
    Hotkey(Hotkey),
    NewClient { id: u64, writer: UnixStream },
    Msg { id: u64, uid: u32, msg: ClientMsg },
    Gone(u64),
}

pub fn listen(path: &Path) -> Result<UnixListener, Error> {
    if path.exists() {
        match UnixStream::connect(path) {
            Ok(_) => {
                return Err(Error::Msg(
                    "the backlight service is already running".into(),
                ))
            }
            Err(_) => {
                let _ = fs::remove_file(path);
            }
        }
    }
    if let Some(parent) = path.parent() {
        if parent != Path::new("") && parent != Path::new("/") {
            fs::create_dir_all(parent)?;
            fs::set_permissions(parent, fs::Permissions::from_mode(0o755))?;
        }
    }
    let listener = UnixListener::bind(path)?;
    // Any local account may connect. The service is the only process that
    // opens the keyboard, and only root may stop it.
    fs::set_permissions(path, fs::Permissions::from_mode(0o666))?;
    Ok(listener)
}

pub fn serve<F, S>(
    mut config: Config,
    config_path: &Path,
    listener: UnixListener,
    hotkeys: Receiver<Hotkey>,
    mut notice: String,
    mut apply: F,
    mut spawn_ui: S,
) -> Result<(), Error>
where
    F: FnMut(&Profile) -> Result<(), String>,
    S: FnMut(),
{
    let shutdown = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::channel();
    let hotkey_tx = tx.clone();
    thread::spawn(move || {
        while let Ok(hotkey) = hotkeys.recv() {
            if hotkey_tx.send(Work::Hotkey(hotkey)).is_err() {
                break;
            }
        }
    });

    listener.set_nonblocking(true)?;
    let accept_tx = tx.clone();
    let accept_shutdown = Arc::clone(&shutdown);
    let readers: Arc<Mutex<Vec<thread::JoinHandle<()>>>> = Arc::new(Mutex::new(Vec::new()));
    let accept_readers = Arc::clone(&readers);
    let accept_handle = thread::spawn(move || {
        let mut next_id = 1u64;
        loop {
            if accept_shutdown.load(Ordering::SeqCst) {
                break;
            }
            match listener.accept() {
                Ok((stream, _)) => {
                    let id = next_id;
                    next_id += 1;
                    let tx = accept_tx.clone();
                    let reader_shutdown = Arc::clone(&accept_shutdown);
                    let handle =
                        thread::spawn(move || read_client(id, stream, tx, reader_shutdown));
                    accept_readers.lock().unwrap().push(handle);
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(50));
                }
                Err(_) => break,
            }
        }
        let handles = std::mem::take(&mut *accept_readers.lock().unwrap());
        for handle in handles {
            let _ = handle.join();
        }
    });
    drop(tx);

    let mut clients: HashMap<u64, UnixStream> = HashMap::new();
    let mut last_spawn: Option<Instant> = None;
    loop {
        let work = match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(work) => work,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if shutdown.load(Ordering::SeqCst) {
                    break;
                }
                continue;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        match work {
            Work::NewClient { id, writer } => {
                clients.insert(id, writer);
            }
            Work::Gone(id) => {
                clients.remove(&id);
            }
            Work::Hotkey(Hotkey::Cycle) => {
                config.cycle();
                let _ = config.save(config_path);
                publish_apply(&mut config, &mut notice, &mut apply, &mut clients, None);
            }
            Work::Hotkey(Hotkey::Show) => {
                if clients.is_empty() {
                    let due = last_spawn
                        .map(|instant| instant.elapsed() > Duration::from_millis(1500))
                        .unwrap_or(true);
                    if due {
                        spawn_ui();
                        last_spawn = Some(Instant::now());
                    }
                } else {
                    broadcast(&mut clients, None, &show_line()?);
                }
            }
            Work::Msg { id, uid, msg } => match msg {
                ClientMsg::Get => {
                    reply(&mut clients, id, &config_line(&config, &notice)?);
                }
                ClientMsg::Set { config: new_config } => match new_config.check() {
                    Err(error) => {
                        reply(&mut clients, id, &error_line(&error.to_string(), None)?);
                    }
                    Ok(()) => {
                        config = new_config;
                        let _ = config.save(config_path);
                        publish_apply(&mut config, &mut notice, &mut apply, &mut clients, Some(id));
                    }
                },
                ClientMsg::Cycle => {
                    config.cycle();
                    let _ = config.save(config_path);
                    publish_apply(&mut config, &mut notice, &mut apply, &mut clients, Some(id));
                }
                ClientMsg::Stop => {
                    if !may_stop(uid) {
                        reply(
                            &mut clients,
                            id,
                            &error_line("only root can stop the backlight service", None)?,
                        );
                        continue;
                    }
                    reply(
                        &mut clients,
                        id,
                        &serde_json::to_string(&ServerMsg::Stopped)?,
                    );
                    break;
                }
            },
        }
    }

    clients.clear();
    shutdown.store(true, Ordering::SeqCst);
    let _ = accept_handle.join();
    Ok(())
}

fn publish_apply<F>(
    config: &mut Config,
    notice: &mut String,
    apply: &mut F,
    clients: &mut HashMap<u64, UnixStream>,
    requester: Option<u64>,
) where
    F: FnMut(&Profile) -> Result<(), String>,
{
    let result = config
        .selected_profile()
        .map_err(|error| error.to_string())
        .and_then(|profile| apply(profile));
    let line = match result {
        Ok(()) => {
            notice.clear();
            match config_line(config, notice) {
                Ok(line) => line,
                Err(_) => return,
            }
        }
        Err(message) => {
            *notice = message.clone();
            match error_line(&message, Some(config.clone())) {
                Ok(line) => line,
                Err(_) => return,
            }
        }
    };
    if let Some(id) = requester {
        reply(clients, id, &line);
        broadcast(clients, Some(id), &line);
    } else {
        broadcast(clients, None, &line);
    }
}

fn read_client(id: u64, stream: UnixStream, tx: Sender<Work>, shutdown: Arc<AtomicBool>) {
    let uid = peer_uid(&stream);
    let writer = match stream.try_clone() {
        Ok(writer) => writer,
        Err(_) => return,
    };
    if tx.send(Work::NewClient { id, writer }).is_err() {
        return;
    }
    let _ = stream.set_read_timeout(Some(Duration::from_millis(200)));
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    loop {
        if shutdown.load(Ordering::SeqCst) {
            break;
        }
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {
                if line.trim().is_empty() {
                    continue;
                }
                match serde_json::from_str::<ClientMsg>(line.trim()) {
                    Ok(msg) => {
                        if tx.send(Work::Msg { id, uid, msg }).is_err() {
                            break;
                        }
                    }
                    Err(_) => continue,
                }
            }
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock
                    || error.kind() == std::io::ErrorKind::TimedOut =>
            {
                continue;
            }
            Err(_) => break,
        }
    }
    let _ = tx.send(Work::Gone(id));
}

fn config_line(config: &Config, notice: &str) -> Result<String, Error> {
    Ok(serde_json::to_string(&ServerMsg::Config {
        config: config.clone(),
        notice: notice.to_string(),
    })?)
}

fn error_line(message: &str, config: Option<Config>) -> Result<String, Error> {
    Ok(serde_json::to_string(&ServerMsg::Error {
        message: message.to_string(),
        config,
    })?)
}

fn may_stop(uid: u32) -> bool {
    uid == 0 || uid == unsafe { libc::getuid() }
}

fn peer_uid(stream: &UnixStream) -> u32 {
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let rc = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut cred as *mut libc::ucred as *mut libc::c_void,
            &mut len,
        )
    };
    if rc == 0 {
        cred.uid
    } else {
        u32::MAX
    }
}

fn show_line() -> Result<String, Error> {
    Ok(serde_json::to_string(&ServerMsg::Show)?)
}

fn reply(clients: &mut HashMap<u64, UnixStream>, id: u64, line: &str) {
    let Some(stream) = clients.get_mut(&id) else {
        return;
    };
    if writeln!(stream, "{line}").is_err() || stream.flush().is_err() {
        clients.remove(&id);
    }
}

fn broadcast(clients: &mut HashMap<u64, UnixStream>, except: Option<u64>, line: &str) {
    let mut dead = Vec::new();
    for (id, stream) in clients.iter_mut() {
        if Some(*id) == except {
            continue;
        }
        if writeln!(stream, "{line}").is_err() || stream.flush().is_err() {
            dead.push(*id);
        }
    }
    for id in dead {
        clients.remove(&id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::Client;
    use crate::hw::Hotkey;
    use std::sync::atomic::AtomicUsize;

    fn scratch() -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "y720-svc-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn set_cycle_and_show_roundtrip() {
        let dir = scratch();
        let socket = dir.join("sock");
        let config_path = dir.join("config.json");
        let config = Config::default();
        config.save(&config_path).unwrap();
        let listener = UnixListener::bind(&socket).unwrap();
        let (hotkey_tx, hotkey_rx) = mpsc::channel();
        let applied = Arc::new(Mutex::new(Vec::new()));
        let applied_bg = Arc::clone(&applied);
        let shows = Arc::new(AtomicUsize::new(0));
        let shows_bg = Arc::clone(&shows);
        let path = config_path.clone();
        let handle = thread::spawn(move || {
            serve(
                config,
                &path,
                listener,
                hotkey_rx,
                String::new(),
                move |profile| {
                    applied_bg.lock().unwrap().push(profile.clone());
                    Ok(())
                },
                move || {
                    shows_bg.fetch_add(1, Ordering::SeqCst);
                },
            )
            .unwrap();
        });

        let mut client = wait_client(&socket);
        let mut next = Config::load(&config_path).unwrap();
        next.profiles[0].name = "Red".into();
        next.profiles[0].zones[0].color = 3;
        let updated = client.set_config(next).unwrap();
        assert_eq!(updated.profiles[0].name, "Red");
        assert_eq!(applied.lock().unwrap().len(), 1);

        hotkey_tx.send(Hotkey::Show).unwrap();
        thread::sleep(Duration::from_millis(150));
        assert!(client.read().unwrap().is_show());

        hotkey_tx.send(Hotkey::Cycle).unwrap();
        let cycled = client.read().unwrap();
        assert_eq!(cycled.config().unwrap().selected, 0);

        let mut second = Config::load(&config_path).unwrap();
        second.profiles.push(second.profiles[0].clone());
        second.profiles[1].name = "Two".into();
        client.set_config(second).unwrap();
        hotkey_tx.send(Hotkey::Cycle).unwrap();
        let cycled = client.read().unwrap();
        assert_eq!(cycled.config().unwrap().selected, 1);
        assert_eq!(
            Config::load(&config_path).unwrap().profiles[0].zones[0].color,
            3
        );

        client.stop().unwrap();
        drop(hotkey_tx);
        handle.join().unwrap();
        assert_eq!(shows.load(Ordering::SeqCst), 0);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn show_without_a_window_asks_for_one() {
        let dir = scratch();
        let socket = dir.join("sock");
        let config_path = dir.join("config.json");
        let listener = UnixListener::bind(&socket).unwrap();
        let (hotkey_tx, hotkey_rx) = mpsc::channel();
        let shows = Arc::new(AtomicUsize::new(0));
        let shows_bg = Arc::clone(&shows);
        let handle = thread::spawn(move || {
            serve(
                Config::default(),
                &config_path,
                listener,
                hotkey_rx,
                String::new(),
                |_| Ok(()),
                move || {
                    shows_bg.fetch_add(1, Ordering::SeqCst);
                },
            )
            .unwrap();
        });
        let client = wait_client(&socket);
        // Drop the only client so the service has nobody to focus.
        drop(client);
        thread::sleep(Duration::from_millis(400));
        hotkey_tx.send(Hotkey::Show).unwrap();
        let start = Instant::now();
        while shows.load(Ordering::SeqCst) == 0 && start.elapsed() < Duration::from_secs(2) {
            thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(shows.load(Ordering::SeqCst), 1);
        let mut client = wait_client(&socket);
        client.stop().unwrap();
        drop(hotkey_tx);
        handle.join().unwrap();
        let _ = fs::remove_dir_all(dir);
    }

    fn wait_client(socket: &Path) -> Client {
        let start = Instant::now();
        loop {
            if let Ok(client) = Client::connect_path(socket) {
                return client;
            }
            if start.elapsed() > Duration::from_secs(2) {
                panic!("service did not accept a connection");
            }
            thread::sleep(Duration::from_millis(20));
        }
    }
}
