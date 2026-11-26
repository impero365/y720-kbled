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

const BRIGHTNESS: [&str; 4] = ["Off", "Low", "Medium", "High"];
/// Bytes written for those four labels. On this keyboard 3 matches 2, and 5 matches 4.
const BRIGHTNESS_VALUES: [u8; 4] = [0, 1, 2, 4];
const ZONE_NAMES: [&str; 4] = ["Left", "Center", "Right", "Numpad"];
const MODES: [(BacklightMode, &str); 5] = [
    (BacklightMode::AlwaysOn, "Always on"),
    (BacklightMode::Heartbeat, "Heartbeat"),
    (BacklightMode::Breath, "Breath"),
    (BacklightMode::Smooth, "Smooth"),
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
    keyboard: gtk::DrawingArea,
    zone_caption: gtk::Label,
    zone_detail: gtk::Label,
    hover_zone: std::cell::Cell<Option<usize>>,
    color_value: gtk::Label,
    swatches: [gtk::Button; 20],
    brightness: [gtk::Button; 4],
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
        .title("Lenovo y720 keyboard light")
        .default_width(1100)
        .default_height(860)
        .build();
    // A quarter tile on this screen is shorter than 480. A taller minimum
    // makes GTK lay the page out below the tile, so the end of the scroll
    // never comes into view.
    window.set_size_request(420, 240);
    window.set_icon_name(Some("y720-kbled"));

    let page = gtk::Box::new(Orientation::Vertical, 18);
    page.add_css_class("page");
    page.set_margin_top(22);
    page.set_margin_bottom(26);
    page.set_margin_start(26);
    page.set_margin_end(26);

    let header = gtk::Box::new(Orientation::Vertical, 12);
    let titles = gtk::Box::new(Orientation::Vertical, 2);
    let title = gtk::Label::new(Some("Lenovo y720 keyboard light"));
    title.add_css_class("title");
    title.set_halign(Align::Start);
    titles.append(&title);

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

    let keyboard_card = card();
    let keyboard = gtk::DrawingArea::new();
    keyboard.add_css_class("keyboard");
    keyboard.set_hexpand(true);
    keyboard.set_size_request(-1, 280);
    let keyboard_for_size = keyboard.clone();
    let sized = std::cell::Cell::new(0i32);
    keyboard.connect_resize(move |_, width, _| {
        if width < 32 {
            return;
        }
        let layout = keyboard_layout();
        let height = ((width as f64) * layout.height / layout.width).round() as i32;
        let height = height.clamp(200, 380);
        if sized.get() != height {
            sized.set(height);
            keyboard_for_size.set_size_request(-1, height);
        }
    });
    let zone_caption = gtk::Label::new(Some("Click a key"));
    zone_caption.add_css_class("zone-name");
    zone_caption.set_halign(Align::Start);
    zone_caption.set_hexpand(true);
    let zone_detail = gtk::Label::new(None);
    zone_detail.add_css_class("muted");
    zone_detail.set_halign(Align::End);
    let caption_row = gtk::Box::new(Orientation::Horizontal, 12);
    caption_row.append(&zone_caption);
    caption_row.append(&zone_detail);
    keyboard_card.append(&keyboard);
    keyboard_card.append(&caption_row);

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
    let brightness_row = choice_flow(4);
    let brightness: [gtk::Button; 4] = std::array::from_fn(|index| {
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
    page.append(&keyboard_card);
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
        keyboard,
        zone_caption,
        zone_detail,
        hover_zone: std::cell::Cell::new(None),
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

    let ui = Rc::clone(panel);
    panel.keyboard.set_draw_func(move |_, cr, width, height| {
        draw_keyboard(&ui, cr, width, height);
    });

    let click = gtk::GestureClick::new();
    click.set_button(1);
    let ui = Rc::clone(panel);
    click.connect_released(move |_, _, x, y| {
        if let Some(zone) = zone_at(ui.keyboard.width(), ui.keyboard.height(), x, y) {
            ui.selected_zone.set(zone);
            ui.render();
        }
    });
    panel.keyboard.add_controller(click);

    let motion = gtk::EventControllerMotion::new();
    let ui = Rc::clone(panel);
    motion.connect_motion(move |_, x, y| {
        let next = zone_at(ui.keyboard.width(), ui.keyboard.height(), x, y);
        if next != ui.hover_zone.get() {
            ui.hover_zone.set(next);
            ui.keyboard.queue_draw();
            ui.keyboard
                .set_cursor_from_name(next.map(|_| "pointer"));
        }
    });
    let ui = Rc::clone(panel);
    motion.connect_leave(move |_| {
        if ui.hover_zone.get().is_some() {
            ui.hover_zone.set(None);
            ui.keyboard.set_cursor_from_name(None);
            ui.keyboard.queue_draw();
        }
    });
    panel.keyboard.add_controller(motion);

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
                    profile.zones[zone].brightness = BRIGHTNESS_VALUES[index];
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

        let zone = profile.zones[zone_index];
        let color_name = COLORS
            .get(zone.color as usize)
            .map(|(_, name)| *name)
            .unwrap_or("Color");
        let brightness = brightness_name(zone.brightness);
        self.zone_caption
            .set_text(ZONE_NAMES.get(zone_index).copied().unwrap_or("Zone"));
        self.zone_detail
            .set_text(&format!("{color_name} · {brightness}"));
        self.keyboard.queue_draw();

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
                index == brightness_choice(profile.zones[zone_index].brightness),
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

fn brightness_name(value: u8) -> &'static str {
    BRIGHTNESS[brightness_choice(value)]
}

fn brightness_choice(value: u8) -> usize {
    match value {
        0 => 0,
        1 => 1,
        2 | 3 => 2,
        _ => 3,
    }
}

/// 3 is the same step as 2, and 5 is the same step as 4.
fn visible_brightness(value: u8) -> u8 {
    match value {
        0 => 0,
        1 => 1,
        2 | 3 => 2,
        _ => 4,
    }
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
        .zone-name { color: #f4f5f7; font-size: 15px; font-weight: 700; }
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
        button.action, button.primary, button.choice, button.swatch {
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
        dropdown.profile > button:hover {
          background: #2a2e3c;
          background-image: none;
        }
        dropdown.profile popover listview,
        popover.menu listview {
          background-color: #1b1d24;
          background-image: none;
          color: #f4f5f7;
        }
        dropdown.profile popover listview > row,
        popover.menu listview > row {
          background-color: #1b1d24;
          background-image: none;
          color: #f4f5f7;
          border-radius: 8px;
          padding: 6px 10px;
        }
        dropdown.profile popover listview > row:hover,
        popover.menu listview > row:hover,
        popover.menu listview > row.activatable:hover {
          background-color: #5c657e;
          background-image: none;
          color: #ffffff;
        }
        dropdown.profile popover listview > row:selected,
        dropdown.profile popover listview > row:selected:hover,
        popover.menu listview > row:selected,
        popover.menu listview > row:selected:hover,
        popover.menu listview > row.activatable:selected,
        popover.menu listview > row.activatable:selected:hover {
          background: #f4f5f7;
          background-color: #f4f5f7;
          background-image: none;
          color: #14151a;
        }
        dropdown.profile popover listview > row label {
          color: inherit;
        }
        "#,
    );
    for (index, (hex, _)) in COLORS.iter().enumerate() {
        css.push_str(&format!(
            "box.chip.c{index} {{ background: {hex}; background-image: none; }}\n"
        ));
    }
    css
}

const KEY_H: f64 = 40.0;
const KEY_MARGIN: f64 = 5.0;
const BOARD_PAD: f64 = 14.0;
const NUMPAD_GAP: f64 = 18.0;

struct KeyGeom {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    zone: u8,
}

struct KeyboardLayout {
    keys: Vec<KeyGeom>,
    width: f64,
    height: f64,
}

struct ZonePaint {
    rgb: (f64, f64, f64),
    brightness: u8,
    black: bool,
}

fn keyboard_layout() -> &'static KeyboardLayout {
    static LAYOUT: std::sync::OnceLock<KeyboardLayout> = std::sync::OnceLock::new();
    LAYOUT.get_or_init(build_keyboard_layout)
}

fn build_keyboard_layout() -> KeyboardLayout {
    // Widths are the original program's key sizes. Rows share one height.
    const MAIN: [&[f64]; 6] = [
        &[40.0; 14],
        &[
            38.87, 38.87, 38.87, 38.87, 38.87, 38.87, 38.87, 38.87, 38.87, 38.87, 38.87, 38.87,
            38.87, 55.0,
        ],
        &[
            55.0, 38.87, 38.87, 38.87, 38.87, 38.87, 38.87, 38.87, 38.87, 38.87, 38.87, 38.87,
            38.87, 38.87,
        ],
        &[
            75.0, 38.9, 38.9, 38.9, 38.9, 38.9, 38.9, 38.9, 38.9, 38.9, 38.9, 38.9, 67.0,
        ],
        &[
            68.0, 39.7, 39.7, 39.7, 39.7, 39.7, 39.7, 39.7, 39.7, 39.7, 39.7, 115.0,
        ],
        &[55.0, 47.5, 47.5, 47.5, 222.0, 47.5, 47.5, 47.5, 47.5],
    ];
    const NUMPAD: [&[f64]; 6] = [
        &[35.0, 35.0, 35.0, 35.0],
        &[35.0, 35.0, 35.0, 35.0],
        &[35.0, 35.0, 35.0, 35.0],
        &[35.0, 35.0, 35.0, 35.0],
        &[48.0],
        &[48.0, 48.0, 48.0],
    ];

    let (main, main_width) = place_rows(&MAIN, false);
    let (mut numpad, _) = place_rows(&NUMPAD, true);
    let numpad_x = main_width + NUMPAD_GAP;
    for key in &mut numpad {
        key.x += numpad_x;
    }
    let mut keys = main;
    keys.append(&mut numpad);
    for key in &mut keys {
        key.x += BOARD_PAD;
        key.y += BOARD_PAD;
    }
    let width = keys
        .iter()
        .map(|key| key.x + key.w + KEY_MARGIN + BOARD_PAD)
        .fold(0.0, f64::max);
    let height = keys
        .iter()
        .map(|key| key.y + key.h + KEY_MARGIN + BOARD_PAD)
        .fold(0.0, f64::max);
    KeyboardLayout {
        keys,
        width,
        height,
    }
}

fn place_rows(rows: &[&[f64]], numpad: bool) -> (Vec<KeyGeom>, f64) {
    let mut placed = Vec::new();
    for (row, widths) in rows.iter().enumerate() {
        let y = KEY_MARGIN + row as f64 * (KEY_H + KEY_MARGIN * 2.0);
        let mut x = KEY_MARGIN;
        let mut row_keys = Vec::new();
        for (index, width) in widths.iter().enumerate() {
            row_keys.push(KeyGeom {
                x,
                y,
                w: *width,
                h: KEY_H,
                zone: zone_of(numpad, row, index),
            });
            x += width + KEY_MARGIN * 2.0;
        }
        placed.push(row_keys);
    }
    let width = placed
        .iter()
        .flat_map(|row| row.iter())
        .map(|key| key.x + key.w + KEY_MARGIN)
        .fold(0.0, f64::max);
    if numpad {
        for row in &mut placed {
            let row_right = row
                .last()
                .map(|key| key.x + key.w + KEY_MARGIN)
                .unwrap_or(0.0);
            let shift = (width - row_right) / 2.0;
            for key in row {
                key.x += shift;
            }
        }
    }
    (placed.into_iter().flatten().collect(), width)
}

/// Column bands from Izurii's `getKeyRow`. On the bottom row, keys to the
/// right of the spacebar belong to the right zone.
fn zone_of(numpad: bool, row: usize, index: usize) -> u8 {
    if numpad {
        return 3;
    }
    if index < 4 {
        0
    } else if index < 9 {
        if row != 5 || index == 4 { 1 } else { 2 }
    } else {
        2
    }
}

fn frame_for(width: f64, height: f64, layout: &KeyboardLayout) -> (f64, f64, f64) {
    if width <= 1.0 || height <= 1.0 || layout.width <= 0.0 || layout.height <= 0.0 {
        return (1.0, 0.0, 0.0);
    }
    let scale = (width / layout.width).min(height / layout.height);
    let ox = (width - layout.width * scale) / 2.0;
    let oy = (height - layout.height * scale) / 2.0;
    (scale, ox, oy)
}

fn zone_at(width: i32, height: i32, x: f64, y: f64) -> Option<usize> {
    let layout = keyboard_layout();
    let (scale, ox, oy) = frame_for(width as f64, height as f64, layout);
    if scale <= 0.0 {
        return None;
    }
    let px = (x - ox) / scale;
    let py = (y - oy) / scale;
    // Gaps belong to the nearest key, as long as the click is on the board.
    if !(0.0..=layout.width).contains(&px) || !(0.0..=layout.height).contains(&py) {
        return None;
    }
    layout
        .keys
        .iter()
        .min_by(|left, right| {
            key_distance(left, px, py)
                .partial_cmp(&key_distance(right, px, py))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|key| key.zone as usize)
}

fn key_distance(key: &KeyGeom, px: f64, py: f64) -> f64 {
    let dx = if px < key.x {
        key.x - px
    } else if px > key.x + key.w {
        px - (key.x + key.w)
    } else {
        0.0
    };
    let dy = if py < key.y {
        key.y - py
    } else if py > key.y + key.h {
        py - (key.y + key.h)
    } else {
        0.0
    };
    dx.hypot(dy)
}

fn zone_paints(panel: &Panel) -> [ZonePaint; 4] {
    let mut paints = std::array::from_fn(|_| ZonePaint {
        rgb: (0.45, 0.48, 0.55),
        brightness: 2,
        black: false,
    });
    let Some(config) = panel.config.borrow().clone() else {
        return paints;
    };
    let Some(profile) = config.profiles.get(config.selected) else {
        return paints;
    };
    for (index, zone) in profile.zones.iter().enumerate().take(4) {
        let hex = COLORS
            .get(zone.color as usize)
            .map(|(hex, _)| *hex)
            .unwrap_or("#8b909c");
        paints[index] = ZonePaint {
            rgb: parse_hex(hex),
            brightness: visible_brightness(zone.brightness),
            black: zone.color == 19,
        };
    }
    paints
}

fn parse_hex(hex: &str) -> (f64, f64, f64) {
    let hex = hex.trim_start_matches('#');
    let Ok(value) = u32::from_str_radix(hex, 16) else {
        return (0.5, 0.5, 0.5);
    };
    (
        ((value >> 16) & 255) as f64 / 255.0,
        ((value >> 8) & 255) as f64 / 255.0,
        (value & 255) as f64 / 255.0,
    )
}

fn draw_keyboard(panel: &Panel, cr: &gtk::cairo::Context, width: i32, height: i32) {
    let layout = keyboard_layout();
    let (scale, ox, oy) = frame_for(width as f64, height as f64, layout);
    let paints = zone_paints(panel);
    let selected = panel.selected_zone.get();
    let hover = panel.hover_zone.get();

    let _ = cr.save();
    cr.translate(ox, oy);
    cr.scale(scale, scale);

    rounded_rect(cr, 1.0, 1.0, layout.width - 2.0, layout.height - 2.0, 18.0);
    cr.set_source_rgba(0.07, 0.08, 0.10, 1.0);
    let _ = cr.fill_preserve();
    cr.set_source_rgba(0.20, 0.22, 0.27, 1.0);
    cr.set_line_width(1.0);
    let _ = cr.stroke();

    for key in &layout.keys {
        let paint = &paints[key.zone as usize];
        if paint.black || paint.brightness == 0 {
            continue;
        }
        let emphasis = if key.zone as usize == selected {
            1.0
        } else if hover == Some(key.zone as usize) {
            0.72
        } else {
            0.5
        };
        draw_glow(cr, key, paint, emphasis);
    }
    for key in &layout.keys {
        let zone = key.zone as usize;
        draw_key(
            cr,
            key,
            &paints[zone],
            zone == selected,
            hover == Some(zone),
        );
    }
    let _ = cr.restore();
}

fn draw_glow(cr: &gtk::cairo::Context, key: &KeyGeom, paint: &ZonePaint, emphasis: f64) {
    let level = f64::from(paint.brightness) / 5.0;
    let (r, g, b) = paint.rgb;
    for layer in 1..=3 {
        let grow = layer as f64 * (1.4 + 2.4 * level);
        let alpha = emphasis * level * 0.16 / layer as f64;
        rounded_rect(
            cr,
            key.x - grow,
            key.y - grow,
            key.w + grow * 2.0,
            key.h + grow * 2.0,
            6.0 + grow,
        );
        cr.set_source_rgba(r, g, b, alpha);
        let _ = cr.fill();
    }
}

fn draw_key(cr: &gtk::cairo::Context, key: &KeyGeom, paint: &ZonePaint, selected: bool, hover: bool) {
    let (r, g, b) = if paint.black {
        (0.55, 0.57, 0.62)
    } else {
        paint.rgb
    };
    let tint = if paint.black {
        0.0
    } else if selected {
        0.28
    } else if hover {
        0.18
    } else if paint.brightness == 0 {
        0.05
    } else {
        0.12 + 0.08 * f64::from(paint.brightness) / 5.0
    };
    let top = mix_rgb((0.17, 0.18, 0.22), paint.rgb, tint);
    let bottom = mix_rgb((0.09, 0.10, 0.12), paint.rgb, tint * 0.55);

    rounded_rect(cr, key.x, key.y, key.w, key.h, 5.5);
    let gradient = gtk::cairo::LinearGradient::new(key.x, key.y, key.x, key.y + key.h);
    gradient.add_color_stop_rgba(0.0, top.0, top.1, top.2, 1.0);
    gradient.add_color_stop_rgba(1.0, bottom.0, bottom.1, bottom.2, 1.0);
    let _ = cr.set_source(&gradient);
    let _ = cr.fill_preserve();

    let edge_alpha = if paint.brightness == 0 {
        0.38
    } else if selected {
        1.0
    } else {
        0.82
    };
    cr.set_line_width(if selected { 2.0 } else { 1.35 });
    cr.set_source_rgba(r, g, b, edge_alpha);
    let _ = cr.stroke();

    if selected {
        let inset = 3.0;
        rounded_rect(
            cr,
            key.x + inset,
            key.y + inset,
            key.w - inset * 2.0,
            key.h - inset * 2.0,
            3.5,
        );
        cr.set_source_rgba(0.96, 0.96, 0.98, 0.9);
        cr.set_line_width(1.15);
        let _ = cr.stroke();
    }
}

fn mix_rgb(base: (f64, f64, f64), tint: (f64, f64, f64), amount: f64) -> (f64, f64, f64) {
    (
        base.0 + (tint.0 - base.0) * amount,
        base.1 + (tint.1 - base.1) * amount,
        base.2 + (tint.2 - base.2) * amount,
    )
}

fn rounded_rect(cr: &gtk::cairo::Context, x: f64, y: f64, w: f64, h: f64, radius: f64) {
    let radius = radius.min(w / 2.0).min(h / 2.0).max(0.0);
    let pi = std::f64::consts::PI;
    cr.new_sub_path();
    cr.arc(x + w - radius, y + radius, radius, -pi / 2.0, 0.0);
    cr.arc(x + w - radius, y + h - radius, radius, 0.0, pi / 2.0);
    cr.arc(x + radius, y + h - radius, radius, pi / 2.0, pi);
    cr.arc(x + radius, y + radius, radius, pi, pi * 1.5);
    cr.close_path();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_matches_the_keyboard_ranges() {
        assert_eq!(COLORS.len(), 20);
        assert_eq!(BRIGHTNESS.len(), 4);
        assert_eq!(BRIGHTNESS_VALUES, [0, 1, 2, 4]);
        assert_eq!(brightness_name(2), "Medium");
        assert_eq!(brightness_name(3), "Medium");
        assert_eq!(brightness_name(4), "High");
        assert_eq!(brightness_name(5), "High");
        assert_eq!(visible_brightness(3), 2);
        assert_eq!(visible_brightness(5), 4);
        assert_eq!(MODES.len(), 5);
        assert_eq!(ZONE_NAMES.len(), 4);
        assert!(stylesheet().contains("box.chip.c19"));
        assert!(stylesheet().contains("#e51012"));
    }

    #[test]
    fn keyboard_zones_follow_the_original_columns() {
        assert_eq!(zone_of(false, 0, 0), 0);
        assert_eq!(zone_of(false, 0, 3), 0);
        assert_eq!(zone_of(false, 2, 4), 1);
        assert_eq!(zone_of(false, 2, 8), 1);
        assert_eq!(zone_of(false, 2, 9), 2);
        assert_eq!(zone_of(false, 4, 13), 2);
        // The spacebar stays in the center zone. Keys to its right do not.
        assert_eq!(zone_of(false, 5, 4), 1);
        assert_eq!(zone_of(false, 5, 5), 2);
        assert_eq!(zone_of(false, 5, 8), 2);
        assert_eq!(zone_of(true, 4, 0), 3);

        let layout = keyboard_layout();
        assert_eq!(layout.keys.iter().filter(|key| key.zone == 3).count(), 20);
        assert_eq!(layout.keys.len(), 76 + 20);

        let width = layout.width.ceil() as i32;
        let height = layout.height.ceil() as i32;
        let (scale, ox, oy) = frame_for(width as f64, height as f64, layout);
        for key in &layout.keys {
            let x = ox + (key.x + key.w / 2.0) * scale;
            let y = oy + (key.y + key.h / 2.0) * scale;
            assert_eq!(zone_at(width, height, x, y), Some(key.zone as usize));
        }
        let left = &layout.keys[0];
        let next = &layout.keys[1];
        let gap_x = ox + ((left.x + left.w + next.x) / 2.0) * scale;
        let gap_y = oy + (left.y + left.h / 2.0) * scale;
        assert!(gap_x > ox + (left.x + left.w) * scale && gap_x < ox + next.x * scale);
        assert_eq!(zone_at(width, height, gap_x, gap_y), Some(left.zone as usize));
        assert_eq!(zone_at(width, height, 0.0, 0.0), None);
    }
}
