use std::io::{Cursor, Write};
use std::os::unix::net::UnixStream;
use std::rc::Rc;
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use gtk::glib;
use gtk::prelude::*;
use gtk::{Align, Orientation};

use crate::cli::connect_service;
use crate::config::{BacklightMode, Config};
use crate::error::Error;
use crate::proto::{ClientMsg, ServerMsg};

const COLORS: [(&str, &str); 20] = [
    ("#e51012", "Red"),
    ("#fd0154", "Torch red"),
    ("#ff00aa", "Hollywood cerise"),
    ("#ff00fe", "Magenta"),
    ("#aa00ff", "Electric violet"),
    ("#5500fe", "Electric indigo"),
    ("#0000ff", "Blue"),
    ("#0055fe", "Blue ribbon"),
    ("#00aaff", "Azure"),
    ("#00ffff", "Cyan"),
    ("#00ffab", "Spring green"),
    ("#00ff55", "Spring green 2"),
    ("#01ff00", "Green"),
    ("#55ff00", "Bright green"),
    ("#aaff01", "Lime"),
    ("#ffff00", "Yellow"),
    ("#ffaa01", "Orange"),
    ("#ff5306", "International orange"),
    ("#ffffff", "White"),
    ("#000000", "Black"),
];

const BRIGHTNESS: [&str; 6] = ["Off", "Low", "Medium", "High", "Ultra", "Enough"];
const ZONE_NAMES: [&str; 4] = ["Left", "Center left", "Center right", "Numpad"];
const MODES: [(BacklightMode, &str); 5] = [
    (BacklightMode::Heartbeat, "Heartbeat"),
    (BacklightMode::Breath, "Breath"),
    (BacklightMode::Smooth, "Smooth"),
    (BacklightMode::AlwaysOn, "Always on"),
    (BacklightMode::Wave, "Wave"),
];

struct Panel {
    config: std::cell::RefCell<Option<Config>>,
    selected_zone: std::cell::Cell<usize>,
    refreshing: std::cell::Cell<bool>,
    writer: Arc<Mutex<Option<UnixStream>>>,
    window: gtk::ApplicationWindow,
    notice: gtk::Label,
    profiles: gtk::DropDown,
    profile_model: gtk::StringList,
    add_profile: gtk::Button,
    rename_profile: gtk::Button,
    delete_profile: gtk::Button,
    apply_brightness: gtk::Button,
    zone_buttons: [gtk::Button; 4],
    zone_details: [gtk::Label; 4],
    color_value: gtk::Label,
    swatches: [gtk::Button; 20],
    brightness: [gtk::Button; 6],
    modes: [gtk::Button; 5],
}

enum UiEvent {
    Server(ServerMsg),
    Notice(String),
    Present,
    Quit,
}

struct AppTray {
    events: Sender<UiEvent>,
    icon: Vec<ksni::Icon>,
}

impl ksni::Tray for AppTray {
    fn id(&self) -> String {
        "y720-kbled".into()
    }

    fn title(&self) -> String {
        "Y720 Keyboard LEDs".into()
    }

    fn icon_name(&self) -> String {
        "y720-kbled".into()
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        self.icon.clone()
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        let _ = self.events.send(UiEvent::Present);
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        vec![
            ksni::menu::StandardItem {
                label: "Show".into(),
                activate: Box::new(|tray: &mut Self| {
                    let _ = tray.events.send(UiEvent::Present);
                }),
                ..Default::default()
            }
            .into(),
            ksni::menu::StandardItem {
                label: "Quit".into(),
                activate: Box::new(|tray: &mut Self| {
                    let _ = tray.events.send(UiEvent::Quit);
                }),
                ..Default::default()
            }
            .into(),
        ]
    }
}

pub fn run() -> Result<(), Error> {
    let app = gtk::Application::builder()
        .application_id("dev.y720.kbled")
        .build();
    let (events, incoming) = mpsc::channel();
    let incoming = Rc::new(std::cell::RefCell::new(Some(incoming)));
    let writer = Arc::new(Mutex::new(None));
    let window: Rc<std::cell::RefCell<Option<gtk::ApplicationWindow>>> =
        Rc::new(std::cell::RefCell::new(None));
    let started = Rc::new(std::cell::Cell::new(false));

    app.connect_activate(move |app| {
        if started.get() {
            if let Some(window) = window.borrow().as_ref() {
                present(window);
            }
            return;
        }
        started.set(true);
        install_style();

        let panel = build_panel(app, Arc::clone(&writer));
        *window.borrow_mut() = Some(panel.window.clone());

        let tray_ok = panel_tray(events.clone());
        // The guard must stay alive. Dropping it releases the hold, and the
        // process would exit when the window hides. Numpad 7 can then only
        // present this window while the tray is running.
        let hold = tray_ok.then(|| app.hold());
        let panel_window = panel.window.clone();
        panel.window.connect_close_request(move |window| {
            if tray_ok {
                window.set_visible(false);
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });

        let panel = Rc::new(panel);
        wire_controls(&panel);
        present(&panel_window);

        let writer = Arc::clone(&writer);
        let events_for_service = events.clone();
        thread::spawn(move || service_loop(events_for_service, writer));

        let panel_for_events = Rc::clone(&panel);
        let Some(incoming) = incoming.borrow_mut().take() else {
            return;
        };
        glib::timeout_add_local(Duration::from_millis(16), move || {
            // `hold` has to outlive this callback. Dropping it lets GTK quit
            // when the window is hidden.
            let _hold = &hold;
            while let Ok(event) = incoming.try_recv() {
                match event {
                    UiEvent::Server(message) => apply_server(&panel_for_events, message),
                    UiEvent::Notice(text) => panel_for_events.set_notice(&text),
                    UiEvent::Present => present(&panel_for_events.window),
                    UiEvent::Quit => {
                        if let Some(app) = panel_for_events.window.application() {
                            app.quit();
                        }
                    }
                }
            }
            glib::ControlFlow::Continue
        });
    });

    // clap already parsed `--ui`. GTK must not see those arguments.
    let status = app.run_with_args::<&str>(&[]);
    if status.value() == 0 {
        Ok(())
    } else {
        Err(Error::Msg(format!(
            "the window closed with status {}",
            status.value()
        )))
    }
}

fn install_style() {
    let provider = gtk::CssProvider::new();
    provider.load_from_string(&stylesheet());
    if let Some(display) = gtk::gdk::Display::default() {
        // User priority beats the desktop theme's button background, which
        // otherwise paints over these colors.
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_USER,
        );
    }
}

fn build_panel(app: &gtk::Application, writer: Arc<Mutex<Option<UnixStream>>>) -> Panel {
    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .title("Y720 Keyboard LEDs")
        .default_width(1100)
        .default_height(700)
        .build();
    window.set_size_request(640, 400);
    window.set_icon_name(Some("y720-kbled"));

    let page = gtk::Box::new(Orientation::Vertical, 18);
    page.add_css_class("page");
    page.set_margin_top(22);
    page.set_margin_bottom(26);
    page.set_margin_start(26);
    page.set_margin_end(26);

    let header = gtk::Box::new(Orientation::Vertical, 12);
    let titles = gtk::Box::new(Orientation::Vertical, 2);
    let title = gtk::Label::new(Some("Keyboard light"));
    title.add_css_class("title");
    title.set_halign(Align::Start);
    let subtitle = gtk::Label::new(Some(
        "Four zones, from the left of the keyboard to the numpad.",
    ));
    subtitle.add_css_class("muted");
    subtitle.set_halign(Align::Start);
    subtitle.set_wrap(true);
    titles.append(&title);
    titles.append(&subtitle);

    let profile_model = gtk::StringList::new(&[]);
    let expression = gtk::PropertyExpression::new(
        gtk::StringObject::static_type(),
        None::<gtk::Expression>,
        "string",
    );
    let profiles = gtk::DropDown::builder()
        .model(&profile_model)
        .expression(&expression)
        .build();
    profiles.set_hexpand(true);
    profiles.add_css_class("profile");
    let add = gtk::Button::with_label("Add");
    let rename = gtk::Button::with_label("Rename");
    let delete_profile = gtk::Button::with_label("Delete");
    for button in [&add, &rename, &delete_profile] {
        button.add_css_class("action");
    }
    delete_profile.set_sensitive(false);
    let profile_row = gtk::Box::new(Orientation::Horizontal, 8);
    profile_row.append(&profiles);
    profile_row.append(&add);
    profile_row.append(&rename);
    profile_row.append(&delete_profile);
    header.append(&titles);
    header.append(&profile_row);

    let notice = gtk::Label::new(None);
    notice.add_css_class("notice");
    notice.set_halign(Align::Start);
    notice.set_xalign(0.0);
    notice.set_wrap(true);
    notice.set_visible(false);

    let zones = gtk::Box::new(Orientation::Horizontal, 10);
    zones.set_homogeneous(true);
    let mut zone_details: [Option<gtk::Label>; 4] = std::array::from_fn(|_| None);
    let zone_buttons: [gtk::Button; 4] = std::array::from_fn(|index| {
        let button = gtk::Button::new();
        button.add_css_class("zone");
        button.set_hexpand(true);
        let text = gtk::Box::new(Orientation::Vertical, 4);
        text.set_halign(Align::Start);
        let name = gtk::Label::new(Some(ZONE_NAMES[index]));
        name.add_css_class("zone-name");
        name.set_halign(Align::Start);
        let detail = gtk::Label::new(Some(""));
        detail.add_css_class("muted");
        detail.set_halign(Align::Start);
        text.append(&name);
        text.append(&detail);
        button.set_child(Some(&text));
        zones.append(&button);
        zone_details[index] = Some(detail);
        button
    });
    let zone_details = zone_details.map(|label| label.expect("zone detail"));
    let hint = gtk::Label::new(Some(
        "Choose a zone, then a color and a brightness. The zone itself does not change the keyboard.",
    ));
    hint.add_css_class("muted");
    hint.set_halign(Align::Start);
    hint.set_wrap(true);

    let color_card = card();
    let color_heading = gtk::Box::new(Orientation::Horizontal, 12);
    color_heading.append(&section("Color"));
    let color_value = gtk::Label::new(None);
    color_value.add_css_class("muted");
    color_value.set_hexpand(true);
    color_value.set_halign(Align::End);
    color_heading.append(&color_value);
    let swatch_row = gtk::Grid::new();
    swatch_row.set_column_spacing(8);
    swatch_row.set_row_spacing(8);
    swatch_row.set_halign(Align::Start);
    let swatches: [gtk::Button; 20] = std::array::from_fn(|index| {
        let button = gtk::Button::new();
        button.add_css_class("swatch");
        button.set_tooltip_text(Some(COLORS[index].1));
        button.set_size_request(40, 40);
        button.set_hexpand(false);
        button.set_vexpand(false);
        let chip = gtk::Box::new(Orientation::Horizontal, 0);
        chip.add_css_class("chip");
        chip.add_css_class(&format!("c{index}"));
        chip.set_size_request(28, 28);
        button.set_child(Some(&chip));
        swatch_row.attach(&button, (index % 10) as i32, (index / 10) as i32, 1, 1);
        button
    });
    color_card.append(&color_heading);
    color_card.append(&swatch_row);

    let brightness_card = card();
    brightness_card.append(&section("Brightness"));
    let brightness_row = choice_flow(6);
    let brightness: [gtk::Button; 6] = std::array::from_fn(|index| {
        let button = gtk::Button::with_label(BRIGHTNESS[index]);
        button.add_css_class("choice");
        brightness_row.insert(&button, -1);
        button
    });
    let apply_brightness = gtk::Button::with_label("Apply this brightness to every zone");
    apply_brightness.add_css_class("action");
    apply_brightness.set_hexpand(true);
    brightness_card.append(&brightness_row);
    brightness_card.append(&apply_brightness);

    let style_card = card();
    style_card.append(&section("Style"));
    let mode_row = choice_flow(3);
    let modes: [gtk::Button; 5] = std::array::from_fn(|index| {
        let button = gtk::Button::with_label(MODES[index].1);
        button.add_css_class("choice");
        mode_row.insert(&button, -1);
        button
    });
    style_card.append(&mode_row);

    let split = gtk::Box::new(Orientation::Horizontal, 16);
    split.set_homogeneous(true);
    brightness_card.set_hexpand(true);
    style_card.set_hexpand(true);
    split.append(&brightness_card);
    split.append(&style_card);

    page.append(&header);
    page.append(&notice);
    page.append(&zones);
    page.append(&hint);
    page.append(&color_card);
    page.append(&split);

    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .child(&page)
        .build();
    window.set_child(Some(&scroll));

    let panel = Panel {
        config: std::cell::RefCell::new(None),
        selected_zone: std::cell::Cell::new(0),
        refreshing: std::cell::Cell::new(false),
        writer,
        window,
        notice,
        profiles,
        profile_model,
        add_profile: add,
        rename_profile: rename,
        delete_profile,
        apply_brightness,
        zone_buttons,
        zone_details,
        color_value,
        swatches,
        brightness,
        modes,
    };

    panel
}

fn wire_controls(panel: &Rc<Panel>) {
    let ui = Rc::clone(panel);
    panel.profiles.connect_selected_notify(move |dropdown| {
        if ui.refreshing.get() {
            return;
        }
        let index = dropdown.selected() as usize;
        let valid = ui
            .config
            .borrow()
            .as_ref()
            .is_some_and(|config| index < config.profiles.len() && index != config.selected);
        if !valid {
            return;
        }
        ui.selected_zone.set(0);
        with_config(&ui, |config| {
            config.selected = index;
        });
    });

    for (index, button) in panel.zone_buttons.iter().enumerate() {
        let ui = Rc::clone(panel);
        button.connect_clicked(move |_| {
            ui.selected_zone.set(index);
            ui.render();
        });
    }

    for (index, button) in panel.swatches.iter().enumerate() {
        let ui = Rc::clone(panel);
        button.connect_clicked(move |_| {
            with_config(&ui, |config| {
                let zone = ui.selected_zone.get();
                if let Some(profile) = config.profiles.get_mut(config.selected) {
                    profile.zones[zone].color = index as u8;
                }
            });
        });
    }

    for (index, button) in panel.brightness.iter().enumerate() {
        let ui = Rc::clone(panel);
        button.connect_clicked(move |_| {
            with_config(&ui, |config| {
                let zone = ui.selected_zone.get();
                if let Some(profile) = config.profiles.get_mut(config.selected) {
                    profile.zones[zone].brightness = index as u8;
                }
            });
        });
    }

    for (index, button) in panel.modes.iter().enumerate() {
        let ui = Rc::clone(panel);
        button.connect_clicked(move |_| {
            with_config(&ui, |config| {
                if let Some(profile) = config.profiles.get_mut(config.selected) {
                    profile.mode = MODES[index].0;
                }
            });
        });
    }

    let ui = Rc::clone(panel);
    panel.add_profile.connect_clicked(move |_| {
        let suggested = ui
            .config
            .borrow()
            .as_ref()
            .map(|config| format!("Profile {}", config.profiles.len() + 1))
            .unwrap_or_else(|| "Profile".into());
        let ui = Rc::clone(&ui);
        let parent = ui.window.clone();
        ask_name(&parent, "New profile", &suggested, move |name| {
            with_config(&ui, |config| {
                let mut copy = config.profiles[config.selected].clone();
                copy.name = name;
                config.profiles.push(copy);
                config.selected = config.profiles.len() - 1;
            });
        });
    });

    let ui = Rc::clone(panel);
    panel.rename_profile.connect_clicked(move |_| {
        let current = ui
            .config
            .borrow()
            .as_ref()
            .and_then(|config| config.profiles.get(config.selected))
            .map(|profile| profile.name.clone())
            .unwrap_or_default();
        let ui = Rc::clone(&ui);
        let parent = ui.window.clone();
        ask_name(&parent, "Rename profile", &current, move |name| {
            with_config(&ui, |config| {
                if let Some(profile) = config.profiles.get_mut(config.selected) {
                    profile.name = name;
                }
            });
        });
    });

    let ui = Rc::clone(panel);
    panel.delete_profile.connect_clicked(move |_| {
        with_config(&ui, |config| {
            if config.profiles.len() < 2 {
                return;
            }
            let index = config.selected.min(config.profiles.len() - 1);
            config.profiles.remove(index);
            if config.selected >= config.profiles.len() {
                config.selected = config.profiles.len() - 1;
            }
        });
    });

    let ui = Rc::clone(panel);
    panel.apply_brightness.connect_clicked(move |_| {
        with_config(&ui, |config| {
            let Some(profile) = config.profiles.get_mut(config.selected) else {
                return;
            };
            let zone = ui.selected_zone.get().min(profile.zones.len() - 1);
            let level = profile.zones[zone].brightness;
            for zone in &mut profile.zones {
                zone.brightness = level;
            }
        });
    });
}

impl Panel {
    fn set_notice(&self, text: &str) {
        let text = text.trim();
        self.notice.set_text(text);
        self.notice.set_visible(!text.is_empty());
    }

    fn render(&self) {
        let Some(config) = self.config.borrow().clone() else {
            return;
        };
        let Some(profile) = config.profiles.get(config.selected) else {
            return;
        };
        let zone_index = self.selected_zone.get().min(profile.zones.len() - 1);

        self.refreshing.set(true);
        while self.profile_model.n_items() > 0 {
            self.profile_model.remove(0);
        }
        for profile in &config.profiles {
            self.profile_model.append(&profile.name);
        }
        self.profiles.set_selected(config.selected as u32);
        self.refreshing.set(false);
        self.delete_profile
            .set_sensitive(config.profiles.len() >= 2);

        for (index, button) in self.zone_buttons.iter().enumerate() {
            let zone = profile.zones[index];
            set_color_class(button, zone.color);
            set_on(button, index == zone_index);
            let color_name = COLORS
                .get(zone.color as usize)
                .map(|(_, name)| *name)
                .unwrap_or("Color");
            let brightness = BRIGHTNESS
                .get(zone.brightness as usize)
                .copied()
                .unwrap_or("");
            self.zone_details[index].set_text(&format!("{color_name} · {brightness}"));
        }

        let (hex, color_name) = COLORS
            .get(profile.zones[zone_index].color as usize)
            .copied()
            .unwrap_or(("", ""));
        self.color_value.set_text(&format!("{color_name}  {hex}"));
        for (index, button) in self.swatches.iter().enumerate() {
            set_on(button, index == profile.zones[zone_index].color as usize);
        }
        for (index, button) in self.brightness.iter().enumerate() {
            set_on(
                button,
                index == profile.zones[zone_index].brightness as usize,
            );
        }
        for (index, button) in self.modes.iter().enumerate() {
            set_on(button, MODES[index].0 == profile.mode);
        }
    }
}

fn with_config(panel: &Panel, change: impl FnOnce(&mut Config)) {
    let ready = {
        let mut guard = panel.config.borrow_mut();
        if let Some(config) = guard.as_mut() {
            change(config);
            true
        } else {
            false
        }
    };
    if ready {
        panel.push();
    } else {
        panel.set_notice("the backlight service did not send a profile");
    }
}

impl Panel {
    fn push(&self) {
        let Some(config) = self.config.borrow().clone() else {
            self.set_notice("the backlight service did not send a profile");
            return;
        };
        if let Err(error) = config.check() {
            self.set_notice(&error.to_string());
            return;
        }
        self.set_notice("");
        self.render();
        if let Err(error) = write_line(&self.writer, &ClientMsg::Set { config }) {
            self.set_notice(&error);
        }
    }
}

fn apply_server(panel: &Panel, message: ServerMsg) {
    match message {
        ServerMsg::Config { config, notice } => {
            *panel.config.borrow_mut() = Some(config);
            panel.set_notice(&notice);
            panel.render();
        }
        ServerMsg::Error { message, config } => {
            if let Some(config) = config {
                *panel.config.borrow_mut() = Some(config);
                panel.render();
            }
            panel.set_notice(&message);
        }
        ServerMsg::Show => present(&panel.window),
        ServerMsg::Stopped => panel.set_notice("the backlight service stopped."),
    }
}

fn present(window: &gtk::ApplicationWindow) {
    window.unminimize();
    window.set_visible(true);
    window.present();
}

fn ask_name(
    parent: &impl IsA<gtk::Window>,
    title: &str,
    initial: &str,
    on_ok: impl FnOnce(String) + 'static,
) {
    let on_ok = Rc::new(std::cell::RefCell::new(Some(on_ok)));
    let dialog = gtk::Window::builder()
        .transient_for(parent)
        .modal(true)
        .destroy_with_parent(true)
        .title(title)
        .default_width(420)
        .build();
    dialog.add_css_class("name-dialog");

    let content = gtk::Box::new(Orientation::Vertical, 12);
    content.set_margin_top(18);
    content.set_margin_bottom(18);
    content.set_margin_start(18);
    content.set_margin_end(18);
    let entry = gtk::Entry::new();
    entry.set_max_length(40);
    entry.set_text(initial);
    entry.set_placeholder_text(Some("Profile name"));
    entry.set_activates_default(true);
    let buttons = gtk::Box::new(Orientation::Horizontal, 8);
    buttons.set_halign(Align::End);
    let cancel = gtk::Button::with_label("Cancel");
    cancel.add_css_class("action");
    let save = gtk::Button::with_label("Save");
    save.add_css_class("primary");
    save.set_receives_default(true);
    buttons.append(&cancel);
    buttons.append(&save);
    content.append(&entry);
    content.append(&buttons);
    dialog.set_child(Some(&content));
    dialog.set_default_widget(Some(&save));

    let dialog_save = dialog.clone();
    let entry_save = entry.clone();
    save.connect_clicked(move |_| {
        let name = entry_save.text().trim().to_string();
        dialog_save.close();
        if name.is_empty() {
            return;
        }
        if let Some(on_ok) = on_ok.borrow_mut().take() {
            on_ok(name);
        }
    });
    let dialog_cancel = dialog.clone();
    cancel.connect_clicked(move |_| dialog_cancel.close());
    dialog.present();
    entry.grab_focus();
}

fn service_loop(events: Sender<UiEvent>, writer: Arc<Mutex<Option<UnixStream>>>) {
    let mut client = match connect_service() {
        Ok(client) => client,
        Err(error) => {
            let _ = events.send(UiEvent::Notice(error.to_string()));
            return;
        }
    };
    if let Err(error) = client.set_read_timeout(None) {
        let _ = events.send(UiEvent::Notice(error.to_string()));
        return;
    }
    match client.clone_stream() {
        Ok(stream) => *writer.lock().unwrap() = Some(stream),
        Err(error) => {
            let _ = events.send(UiEvent::Notice(error.to_string()));
            return;
        }
    }
    if let Err(error) = write_line(&writer, &ClientMsg::Get) {
        let _ = events.send(UiEvent::Notice(error));
        return;
    }
    loop {
        match client.read() {
            Ok(message) => {
                let _ = events.send(UiEvent::Server(message));
            }
            Err(Error::Io(error))
                if error.kind() == std::io::ErrorKind::TimedOut
                    || error.kind() == std::io::ErrorKind::WouldBlock =>
            {
                continue;
            }
            Err(_) => {
                let _ = events.send(UiEvent::Notice(
                    "lost the backlight service. Restart the window to reconnect.".into(),
                ));
                return;
            }
        }
    }
}

fn write_line(writer: &Mutex<Option<UnixStream>>, message: &ClientMsg) -> Result<(), String> {
    let line = serde_json::to_string(message).map_err(|error| error.to_string())?;
    let mut guard = writer
        .lock()
        .map_err(|_| "the connection to the backlight service was lost".to_string())?;
    let stream = guard
        .as_mut()
        .ok_or_else(|| "not connected to the backlight service".to_string())?;
    writeln!(stream, "{line}").map_err(|error| error.to_string())?;
    stream.flush().map_err(|error| error.to_string())
}

fn panel_tray(events: Sender<UiEvent>) -> bool {
    use ksni::blocking::TrayMethods;
    let tray = AppTray {
        events,
        icon: tray_icon(),
    };
    match tray.spawn() {
        Ok(handle) => {
            // The handle owns the tray thread for the life of this process.
            Box::leak(Box::new(handle));
            true
        }
        Err(error) => {
            eprintln!("tray icon unavailable: {error}");
            false
        }
    }
}

fn tray_icon() -> Vec<ksni::Icon> {
    let bytes = include_bytes!("../assets/icon.png");
    let mut decoder = png::Decoder::new(Cursor::new(&bytes[..]));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let reader = decoder.read_info();
    let Ok(mut reader) = reader else {
        return Vec::new();
    };
    let mut buffer = vec![0; reader.output_buffer_size()];
    let Ok(info) = reader.next_frame(&mut buffer) else {
        return Vec::new();
    };
    let pixels = &buffer[..info.buffer_size()];
    if info.color_type != png::ColorType::Rgba || pixels.len() % 4 != 0 {
        return Vec::new();
    }
    let mut data = Vec::with_capacity(pixels.len());
    for pixel in pixels.chunks_exact(4) {
        data.extend_from_slice(&[pixel[3], pixel[0], pixel[1], pixel[2]]);
    }
    vec![ksni::Icon {
        width: info.width as i32,
        height: info.height as i32,
        data,
    }]
}

fn card() -> gtk::Box {
    let card = gtk::Box::new(Orientation::Vertical, 12);
    card.add_css_class("card");
    card
}

fn section(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.add_css_class("section");
    label.set_halign(Align::Start);
    label
}

fn choice_flow(columns: u32) -> gtk::FlowBox {
    let flow = gtk::FlowBox::new();
    flow.set_selection_mode(gtk::SelectionMode::None);
    flow.set_activate_on_single_click(false);
    flow.set_column_spacing(8);
    flow.set_row_spacing(8);
    flow.set_min_children_per_line(2);
    flow.set_max_children_per_line(columns);
    flow.set_halign(Align::Start);
    flow.set_hexpand(true);
    flow.add_css_class("choices");
    flow
}

fn set_on(widget: &impl WidgetExt, on: bool) {
    if on {
        widget.add_css_class("on");
    } else {
        widget.remove_css_class("on");
    }
}

fn set_color_class(widget: &impl WidgetExt, color: u8) {
    for index in 0..COLORS.len() {
        widget.remove_css_class(&format!("c{index}"));
    }
    widget.add_css_class(&format!("c{color}"));
}

fn stylesheet() -> String {
    let mut css = String::from(
        r#"
        window, .page {
          background-color: #101114;
          color: #f4f5f7;
        }
        .page { background-color: #101114; }
        .title {
          font-size: 22px;
          font-weight: 700;
        }
        .section {
          font-size: 15px;
          font-weight: 700;
        }
        .muted { color: #9aa0ad; font-size: 13px; }
        .zone-name { font-size: 15px; font-weight: 700; }
        .card {
          background-color: #171922;
          border: 1px solid #2c2f3a;
          border-radius: 16px;
          padding: 16px;
        }
        .notice {
          background-color: #3a2428;
          color: #ffd5d8;
          border: 1px solid #6e3038;
          border-radius: 12px;
          padding: 10px 12px;
        }
        button.action, button.primary, button.choice, button.zone, button.swatch {
          min-height: 0;
        }
        button.action {
          background: #22252e;
          background-image: none;
          color: #f4f5f7;
          border: 1px solid #3a3d48;
          border-radius: 10px;
          padding: 8px 14px;
        }
        button.action:hover { background: #2c3040; background-image: none; }
        button.action:disabled { opacity: 0.4; }
        button.primary {
          background: #f4f5f7;
          background-image: none;
          color: #14151a;
          border: 1px solid #f4f5f7;
          border-radius: 10px;
          padding: 8px 16px;
          font-weight: 700;
        }
        button.primary:hover { background: #ffffff; background-image: none; }
        dropdown.profile {
          background: #171922;
          background-image: none;
          color: #f4f5f7;
          border: 1px solid #3a3d48;
          border-radius: 10px;
          padding: 2px;
        }
        dropdown.profile > button {
          background: transparent;
          background-image: none;
          color: #f4f5f7;
          border: none;
          box-shadow: none;
        }
        button.zone {
          background: #12141a;
          background-image: none;
          color: #f4f5f7;
          border-style: solid;
          border-width: 1px 1px 1px 4px;
          border-color: #2c2f3a;
          border-radius: 14px;
          padding: 14px 16px;
          min-height: 92px;
        }
        button.zone:hover { background: #1a1d28; background-image: none; }
        button.zone.on {
          background: #1c2030;
          background-image: none;
          border-color: #f4f5f7;
        }
        button.zone label.zone-name { color: #f4f5f7; }
        button.zone label.muted { color: #9aa0ad; }
        button.choice {
          background: #22252e;
          background-image: none;
          color: #f4f5f7;
          border: 1px solid #343846;
          border-radius: 999px;
          padding: 8px 14px;
        }
        button.choice:hover { background: #2c3040; background-image: none; }
        button.choice.on {
          background: #f4f5f7;
          background-image: none;
          color: #14151a;
          border-color: #f4f5f7;
        }
        button.choice.on label { color: #14151a; }
        button.swatch {
          padding: 3px;
          border-radius: 12px;
          background: #171922;
          background-image: none;
          border: 2px solid rgba(255, 255, 255, 0.22);
          min-width: 40px;
          min-height: 40px;
        }
        button.swatch.on { border-color: #ffffff; }
        box.chip { border-radius: 9px; }
        flowbox.choices > flowboxchild {
          padding: 0;
          background: transparent;
          border: none;
        }
        entry {
          background-color: #12141a;
          color: #f4f5f7;
          border: 1px solid #3a3d48;
          border-radius: 10px;
          padding: 8px 10px;
          min-height: 0;
        }
        window.name-dialog { background-color: #1b1d24; }
        popover, popover contents {
          background-color: #1b1d24;
          color: #f4f5f7;
        }
        "#,
    );
    for (index, (hex, _)) in COLORS.iter().enumerate() {
        let rail = if *hex == "#000000" { "#8b909c" } else { hex };
        css.push_str(&format!(
            "button.zone.c{index} {{ border-left-color: {rail}; }}\n\
             button.zone.c{index}.on {{ border-color: #f4f5f7; border-left-color: {rail}; }}\n\
             box.chip.c{index} {{ background: {hex}; background-image: none; }}\n"
        ));
    }
    css
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_matches_the_keyboard_ranges() {
        assert_eq!(COLORS.len(), 20);
        assert_eq!(BRIGHTNESS.len(), 6);
        assert_eq!(MODES.len(), 5);
        assert_eq!(ZONE_NAMES.len(), 4);
        assert!(stylesheet().contains("box.chip.c19"));
        assert!(stylesheet().contains("#e51012"));
    }
}
