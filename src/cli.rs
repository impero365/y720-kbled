use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use clap::{Parser, Subcommand};

use crate::client::Client;
use crate::config::{BacklightMode, Config};
use crate::error::Error;
use crate::hw;
use crate::paths::Paths;
use crate::proc;
use crate::service;
use crate::ui;

#[derive(Parser)]
#[command(
    name = "y720-kbled",
    about = "Control the Lenovo Legion Y720 keyboard backlight"
)]
struct Cli {
    /// Open the settings window.
    #[arg(long)]
    ui: bool,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Apply the selected profile.
    Apply,
    /// Print the LED and keyboard devices.
    Devices,
    /// Print Fn+Space and numpad-7 events until interrupted.
    Watch,
    /// Run the background service.
    Daemon,
    /// Open the settings window.
    Ui,
    /// Stop the background service. Root or the service account only.
    Stop,
    /// Open the window for the person logged in at the keyboard.
    /// The system service runs this as root.
    ShowUi,
    /// Change the selected profile and apply it.
    Set {
        /// heartbeat, breath, smooth, always_on, or wave.
        #[arg(long)]
        mode: Option<String>,
        /// Color index 0-19. Applies to every zone unless --zone is set.
        #[arg(long)]
        color: Option<u8>,
        /// Brightness 0-5. Applies to every zone unless --zone is set.
        #[arg(long)]
        brightness: Option<u8>,
        /// Zone 0-3. Left to right, then the numpad.
        #[arg(long)]
        zone: Option<usize>,
    },
}

pub fn run() -> Result<(), Error> {
    let cli = Cli::parse();
    if cli.ui {
        if cli.command.is_some() {
            return Err(Error::Msg("pass either --ui or a subcommand".into()));
        }
        return ui::run();
    }
    match cli.command {
        Some(Command::Ui) => ui::run(),
        None => Err(Error::Msg(
            "choose a command. Try `y720-kbled --help` or `y720-kbled --ui`".into(),
        )),
        Some(Command::Devices) => {
            println!("{}", hw::device_report());
            Ok(())
        }
        Some(Command::Apply) => apply_current(),
        Some(Command::Watch) => watch(),
        Some(Command::Daemon) => daemon(),
        Some(Command::Stop) => stop(),
        Some(Command::ShowUi) => proc::show_active_user_window(),
        Some(Command::Set {
            mode,
            color,
            brightness,
            zone,
        }) => set(mode, color, brightness, zone),
    }
}

fn daemon() -> Result<(), Error> {
    let paths = Paths::system();
    let path = paths.socket.clone();
    let pid = paths.pid.clone();
    ctrlc::set_handler(move || {
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&pid);
        std::process::exit(0);
    })
    .map_err(|error| Error::Msg(error.to_string()))?;

    let config = Config::load(&paths.config)?;
    let listener = service::listen(&paths.socket)?;
    if let Some(parent) = paths.pid.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&paths.pid, std::process::id().to_string())?;
    let notice = wait_for_devices(&config);
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || loop {
        match hw::watch(tx.clone()) {
            Ok(()) => break,
            Err(error) => {
                eprintln!("hotkey watch stopped: {error}");
                thread::sleep(Duration::from_secs(2));
            }
        }
    });
    let result = service::serve(
        config,
        &paths.config,
        listener,
        rx,
        notice,
        |profile| hw::apply_profile(profile).map_err(|error| error.to_string()),
        proc::open_window,
    );
    let _ = std::fs::remove_file(&paths.socket);
    let _ = std::fs::remove_file(&paths.pid);
    result
}

fn wait_for_devices(config: &Config) -> String {
    let mut notice = String::new();
    for attempt in 0..30 {
        notice.clear();
        match config.selected_profile() {
            Ok(profile) => {
                if let Err(error) = hw::apply_profile(profile) {
                    notice = error.to_string();
                }
            }
            Err(error) => notice = error.to_string(),
        }
        if let Err(error) = hw::keyboard_ready() {
            if !notice.is_empty() {
                notice.push('\n');
            }
            notice.push_str(&error.to_string());
        }
        if notice.is_empty() {
            return String::new();
        }
        if attempt + 1 < 30 {
            thread::sleep(Duration::from_millis(500));
        }
    }
    eprintln!("keyboard is not ready:\n{notice}");
    notice
}

fn stop() -> Result<(), Error> {
    match Client::connect() {
        Ok(mut client) => client.stop(),
        Err(error) => Err(Error::Msg(format!(
            "the backlight service is not running ({error})"
        ))),
    }
}

fn apply_current() -> Result<(), Error> {
    let mut client = connect_service()?;
    let config = client.get_config()?;
    let _ = client.set_config(config)?;
    Ok(())
}

fn set(
    mode: Option<String>,
    color: Option<u8>,
    brightness: Option<u8>,
    zone: Option<usize>,
) -> Result<(), Error> {
    if mode.is_none() && color.is_none() && brightness.is_none() {
        return Err(Error::Msg(
            "set needs at least one of --mode, --color, or --brightness".into(),
        ));
    }
    let mode = mode.map(|value| BacklightMode::parse(&value)).transpose()?;
    with_config(|config| {
        let profile = config.selected_profile_mut()?;
        if let Some(mode) = mode {
            profile.mode = mode;
        }
        let zones: Vec<usize> = match zone {
            Some(index) if index < profile.zones.len() => vec![index],
            Some(index) => {
                return Err(Error::Msg(format!(
                    "zone must be 0-{}, got {index}",
                    profile.zones.len() - 1
                )))
            }
            None => (0..profile.zones.len()).collect(),
        };
        for index in zones {
            if let Some(color) = color {
                profile.zones[index].color = color;
            }
            if let Some(brightness) = brightness {
                profile.zones[index].brightness = brightness;
            }
        }
        Ok(())
    })
}

fn with_config(change: impl FnOnce(&mut Config) -> Result<(), Error>) -> Result<(), Error> {
    let mut client = connect_service()?;
    let mut config = client.get_config()?;
    change(&mut config)?;
    let _ = client.set_config(config)?;
    Ok(())
}

fn watch() -> Result<(), Error> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        if let Err(error) = hw::watch(tx) {
            eprintln!("{error}");
        }
    });
    eprintln!("watching Fn+Space and numpad 7. Press Ctrl-C to stop.");
    while let Ok(hotkey) = rx.recv() {
        println!("{hotkey:?}");
    }
    Ok(())
}

pub fn connect_service() -> Result<Client, Error> {
    Client::connect().map_err(|_| {
        Error::Msg(
            "the backlight service is not running. Start it with: sudo systemctl enable --now y720-kbled.service"
                .into(),
        )
    })
}
