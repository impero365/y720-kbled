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
use crate::config::{BacklightMode, Config, Profile};
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

/// Black (off) and white first, then the rainbow. Indexes into `COLORS`.
const SWATCH_ORDER: [usize; 20] = [
    19, 18, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17,
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
    brightness_hint: gtk::Label,
    /// Hidden while the Off profile is selected.
    lit_controls: [gtk::Widget; 3],
    hover_zone: std::cell::Cell<Option<usize>>,
    status: gtk::Label,
    accent: gtk::CssProvider,
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
        "Lenovo Y720 Keyboard Backlight".into()
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
                    UiEvent::Notice(text) => {
                        // Only the connection thread sends these, while it is disconnected.
                        panel_for_events.set_status(false);
                        panel_for_events.set_notice(&text);
                    }
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
        .title("Lenovo Y720 Keyboard Backlight")
        .default_width(1100)
        .default_height(860)
        .build();
    // A quarter tile on this screen is shorter than 480. A taller minimum
    // makes GTK lay the page out below the tile, so the end of the scroll
    // never comes into view.
    window.set_size_request(420, 240);
    window.set_icon_name(Some("y720-kbled"));

    let page = gtk::Box::new(Orientation::Vertical, 6);
    page.add_css_class("page");
    page.set_margin_top(8);
    page.set_margin_bottom(8);
    page.set_margin_start(8);
    page.set_margin_end(8);

    let header = gtk::Box::new(Orientation::Horizontal, 8);
    header.add_css_class("header");
    let logo = gtk::DrawingArea::new();
    logo.set_content_width(18);
    logo.set_content_height(18);
    logo.set_valign(Align::Center);
    logo.set_draw_func(|_, cr, width, height| draw_palette(cr, width, height));
    let title = gtk::Label::new(Some("Lenovo Y720 Keyboard Backlight"));
    title.add_css_class("title");
    title.set_halign(Align::Start);
    title.set_valign(Align::Center);
    title.set_hexpand(true);
    title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    let status = gtk::Label::new(Some("Connecting"));
    status.add_css_class("status");
    status.set_valign(Align::Center);
    header.append(&logo);
    header.append(&title);
    header.append(&status);

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
    // Its own height, not stretched to the tallest button in the bar.
    profiles.set_valign(Align::Center);
    let add = icon_button("list-add-symbolic", "New", "Copy this profile into a new one");
    let rename = icon_button("document-edit-symbolic", "Rename", "Rename this profile");
    let delete_profile = icon_button("user-trash-symbolic", "Delete", "Delete this profile");
    delete_profile.add_css_class("danger");
    delete_profile.set_sensitive(false);
    let profile_label = gtk::Label::new(Some("PROFILE"));
    profile_label.add_css_class("eyebrow");
    let profile_row = gtk::Box::new(Orientation::Horizontal, 4);
    profile_row.add_css_class("toolbar");
    profile_row.append(&profile_label);
    profile_row.append(&profiles);
    profile_row.append(&add);
    profile_row.append(&rename);
    profile_row.append(&delete_profile);

    let notice = gtk::Label::new(None);
    notice.add_css_class("notice");
    notice.set_halign(Align::Fill);
    notice.set_xalign(0.0);
    notice.set_wrap(true);
    notice.set_visible(false);

    // No card around the keyboard: the drawing has its own chassis.
    let keyboard_card = gtk::Box::new(Orientation::Vertical, 6);
    let keyboard = gtk::DrawingArea::new();
    keyboard.add_css_class("keyboard");
    keyboard.set_hexpand(true);
    keyboard.set_size_request(-1, 200);
    let keyboard_for_size = keyboard.clone();
    let sized = Rc::new(std::cell::Cell::new(0i32));
    keyboard.connect_resize(move |_, width, _| {
        if width < 32 {
            return;
        }
        let layout = keyboard_layout();
        let height = ((width as f64) * layout.height / layout.width).round() as i32;
        let height = height.clamp(140, 380);
        if sized.get() != height {
            sized.set(height);
            // A size request made during allocation is dropped. Apply it on
            // the next idle so a narrower window also gets a shorter board.
            let keyboard = keyboard_for_size.clone();
            glib::idle_add_local_once(move || keyboard.set_size_request(-1, height));
        }
    });
    keyboard_card.append(&keyboard);

    let color_card = card();
    color_card.add_css_class("swatch-card");
    let swatch_row = gtk::FlowBox::new();
    swatch_row.set_selection_mode(gtk::SelectionMode::None);
    swatch_row.set_homogeneous(true);
    swatch_row.set_column_spacing(2);
    swatch_row.set_row_spacing(2);
    swatch_row.set_min_children_per_line(2);
    swatch_row.set_max_children_per_line(2);
    swatch_row.set_halign(Align::Start);
    swatch_row.add_css_class("choices");
    let swatches: [gtk::Button; 20] = std::array::from_fn(|index| {
        let button = gtk::Button::new();
        button.add_css_class("swatch");
        button.add_css_class(&format!("s{index}"));
        button.set_tooltip_text(Some(COLORS[index].1));
        button.set_halign(Align::Center);
        button.set_valign(Align::Center);
        let chip = gtk::Box::new(Orientation::Horizontal, 0);
        chip.add_css_class("chip");
        chip.add_css_class(&format!("c{index}"));
        chip.set_size_request(22, 22);
        button.set_child(Some(&chip));
        button
    });
    for index in SWATCH_ORDER {
        swatch_row.insert(&swatches[index], -1);
    }
    color_card.append(&swatch_row);

    // The colors sit beside the keyboard, so the page needs one row less.
    keyboard_card.set_hexpand(true);
    // Top-aligned: the keyboard and its color line sit at the top of the
    // column, and any extra height goes below them.
    keyboard_card.set_valign(Align::Start);
    color_card.set_hexpand(false);
    let top = gtk::Box::new(Orientation::Horizontal, 6);
    top.append(&keyboard_card);
    top.append(&color_card);

    // Brightness and Effect are one row each under the keyboard: heading on
    // the left, every choice on a single line beside it.
    let brightness_card = control_row();
    let (brightness_title, brightness_hint) = section("Brightness", "Left zone");
    brightness_title.set_size_request(SECTION_WIDTH, -1);
    let brightness_row = choice_row();
    let brightness: [gtk::Button; 4] = std::array::from_fn(|index| {
        let button = gtk::Button::new();
        button.add_css_class("choice");
        button.add_css_class("level");
        let inner = gtk::Box::new(Orientation::Horizontal, 6);
        inner.set_halign(Align::Center);
        inner.append(&level_bars(index));
        inner.append(&gtk::Label::new(Some(BRIGHTNESS[index])));
        button.set_child(Some(&inner));
        brightness_row.append(&button);
        button
    });
    let apply_brightness = gtk::Button::with_label("All zones");
    apply_brightness.add_css_class("link");
    apply_brightness.set_tooltip_text(Some("Apply this brightness to every zone"));
    apply_brightness.set_valign(Align::Center);
    brightness_card.append(&brightness_title);
    brightness_card.append(&brightness_row);
    brightness_card.append(&apply_brightness);

    let style_card = control_row();
    let (style_title, _) = section("Effect", "Whole keyboard");
    style_title.set_size_request(SECTION_WIDTH, -1);
    let mode_row = choice_row();
    let modes: [gtk::Button; 5] = std::array::from_fn(|index| {
        let button = gtk::Button::with_label(MODES[index].1);
        button.add_css_class("choice");
        mode_row.append(&button);
        button
    });
    style_card.append(&style_title);
    style_card.append(&mode_row);

    keyboard_card.append(&brightness_card);
    keyboard_card.append(&style_card);

    page.append(&header);
    page.append(&profile_row);
    page.append(&notice);
    page.append(&top);

    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .child(&page)
        .build();
    window.set_child(Some(&scroll));

    let accent = gtk::CssProvider::new();
    if let Some(display) = gtk::gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &accent,
            gtk::STYLE_PROVIDER_PRIORITY_USER + 1,
        );
    }

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
        brightness_hint,
        lit_controls: [
            color_card.clone().upcast(),
            brightness_card.clone().upcast(),
            style_card.clone().upcast(),
        ],
        hover_zone: std::cell::Cell::new(None),
        status,
        accent,
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
        if ui.off_selected() {
            return;
        }
        if let Some(zone) = zone_at(ui.keyboard.width(), ui.keyboard.height(), x, y) {
            ui.selected_zone.set(zone);
            ui.render();
        }
    });
    panel.keyboard.add_controller(click);

    let motion = gtk::EventControllerMotion::new();
    let ui = Rc::clone(panel);
    motion.connect_motion(move |_, x, y| {
        let next = if ui.off_selected() {
            None
        } else {
            zone_at(ui.keyboard.width(), ui.keyboard.height(), x, y)
        };
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
                // Off is not a starting point: a profile made from it starts lit.
                let mut copy = match config.profiles.get(config.selected) {
                    Some(profile) if !profile.off => profile.clone(),
                    _ => Profile::lit(&name),
                };
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
                    if !profile.off {
                        profile.name = name;
                    }
                }
            });
        });
    });

    let ui = Rc::clone(panel);
    panel.delete_profile.connect_clicked(move |_| {
        with_config(&ui, |config| {
            let index = config.selected.min(config.profiles.len().saturating_sub(1));
            if config.profiles.get(index).is_none_or(|profile| profile.off) {
                return;
            }
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
    fn off_selected(&self) -> bool {
        self.config
            .borrow()
            .as_ref()
            .and_then(|config| config.profiles.get(config.selected))
            .is_some_and(|profile| profile.off)
    }

    fn set_status(&self, connected: bool) {
        self.status
            .set_text(if connected { "● Service running" } else { "● Not connected" });
        set_on(&self.status, connected);
    }

    fn set_notice(&self, text: &str) {
        let text = sentence_case(text.trim());
        let text = text.as_str();
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
        // Off stays in the list for good, under the same name.
        self.delete_profile.set_sensitive(!profile.off);
        self.rename_profile.set_sensitive(!profile.off);
        for widget in &self.lit_controls {
            widget.set_visible(!profile.off);
        }

        let zone_label = format!(
            "{} zone",
            ZONE_NAMES.get(zone_index).copied().unwrap_or("Selected")
        );
        self.brightness_hint.set_text(&zone_label);
        self.keyboard.queue_draw();

        let zone = profile.zones[zone_index];
        self.accent.load_from_string(&accent_css(zone.color));
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
            panel.set_status(true);
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
        ServerMsg::Stopped => {
            panel.set_status(false);
            panel.set_notice("the backlight service stopped.");
        }
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

/// Keeps the window connected to the service. When the service is not
/// running, or goes away (a package upgrade restarts it), this retries
/// until it is back. The service's first reply clears the notice.
fn service_loop(events: Sender<UiEvent>, writer: Arc<Mutex<Option<UnixStream>>>) {
    let mut delay = RETRY_FIRST;
    let mut last_notice = String::new();
    loop {
        let reason = match run_session(&events, &writer) {
            Ok(()) => return,
            Err(Session::Lost(reason)) => {
                // It was up, so it is likely restarting: retry soon.
                delay = RETRY_FIRST;
                reason
            }
            Err(Session::Failed(reason)) => reason,
        };
        *writer.lock().unwrap() = None;
        let notice = format!("{reason}. Reconnecting…");
        if notice != last_notice {
            if events.send(UiEvent::Notice(notice.clone())).is_err() {
                return;
            }
            last_notice = notice;
        }
        thread::sleep(delay);
        delay = (delay * 2).min(RETRY_MAX);
    }
}

const RETRY_FIRST: Duration = Duration::from_secs(1);
const RETRY_MAX: Duration = Duration::from_secs(5);

enum Session {
    /// Could not connect.
    Failed(String),
    /// Was connected, then the connection dropped.
    Lost(String),
}

/// One connection. Returns `Ok` only when the window is gone.
fn run_session(
    events: &Sender<UiEvent>,
    writer: &Arc<Mutex<Option<UnixStream>>>,
) -> Result<(), Session> {
    let failed = |error: String| Session::Failed(error);
    let mut client = connect_service().map_err(|error| failed(error.to_string()))?;
    client
        .set_read_timeout(None)
        .map_err(|error| failed(error.to_string()))?;
    let stream = client
        .clone_stream()
        .map_err(|error| failed(error.to_string()))?;
    *writer.lock().unwrap() = Some(stream);
    write_line(writer, &ClientMsg::Get).map_err(failed)?;
    loop {
        match client.read() {
            Ok(message) => {
                if events.send(UiEvent::Server(message)).is_err() {
                    return Ok(());
                }
            }
            Err(Error::Io(error))
                if error.kind() == std::io::ErrorKind::TimedOut
                    || error.kind() == std::io::ErrorKind::WouldBlock =>
            {
                continue;
            }
            Err(_) => {
                return Err(Session::Lost("lost the backlight service".into()));
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
    let card = gtk::Box::new(Orientation::Vertical, 14);
    card.add_css_class("card");
    card
}

fn section(text: &str, hint: &str) -> (gtk::Box, gtk::Label) {
    let column = gtk::Box::new(Orientation::Vertical, 2);
    let label = gtk::Label::new(Some(&text.to_uppercase()));
    label.add_css_class("section");
    label.set_halign(Align::Start);
    let hint = gtk::Label::new(Some(hint));
    hint.add_css_class("hint");
    hint.set_halign(Align::Start);
    column.append(&label);
    column.append(&hint);
    (column, hint)
}

fn icon_button(icon: &str, text: &str, tooltip: &str) -> gtk::Button {
    let button = gtk::Button::new();
    button.add_css_class("action");
    button.set_tooltip_text(Some(tooltip));
    let inner = gtk::Box::new(Orientation::Horizontal, 6);
    inner.append(&gtk::Image::from_icon_name(icon));
    inner.append(&gtk::Label::new(Some(text)));
    button.set_child(Some(&inner));
    button
}

/// Four little bars, `level` of them lit, beside each brightness label.
fn level_bars(level: usize) -> gtk::Box {
    let bars = gtk::Box::new(Orientation::Horizontal, 2);
    bars.set_valign(Align::Center);
    bars.add_css_class("bars");
    for step in 0..3 {
        let bar = gtk::Box::new(Orientation::Horizontal, 0);
        bar.add_css_class("bar");
        bar.set_size_request(3, 6 + 3 * step as i32);
        bar.set_valign(Align::End);
        set_on(&bar, step < level);
        bars.append(&bar);
    }
    bars
}

/// A painter's palette with four paint dots, one per zone color family.
fn draw_palette(cr: &gtk::cairo::Context, width: i32, height: i32) {
    let size = f64::from(width.min(height));
    let _ = cr.save();
    cr.translate(
        (f64::from(width) - size) / 2.0,
        (f64::from(height) - size) / 2.0,
    );
    cr.scale(size / 24.0, size / 24.0);
    let pi = std::f64::consts::PI;

    // Body, with a thumb hole cut out by the even-odd rule.
    cr.new_path();
    cr.save().ok();
    cr.translate(12.0, 12.0);
    cr.scale(1.0, 0.86);
    cr.arc(0.0, 0.0, 11.0, 0.0, 2.0 * pi);
    cr.restore().ok();
    cr.new_sub_path();
    cr.arc(15.6, 16.2, 2.3, 0.0, 2.0 * pi);
    cr.set_fill_rule(gtk::cairo::FillRule::EvenOdd);
    cr.set_source_rgb(0.86, 0.88, 0.92);
    let _ = cr.fill();

    for (x, y, hex) in [
        (6.6, 10.6, "#e51012"),
        (10.2, 6.4, "#ffaa01"),
        (15.2, 6.6, "#00ffab"),
        (18.4, 10.6, "#5500fe"),
    ] {
        let (r, g, b) = parse_hex(hex);
        cr.arc(x, y, 2.1, 0.0, 2.0 * pi);
        cr.set_source_rgb(r, g, b);
        let _ = cr.fill();
    }
    let _ = cr.restore();
}

/// Tints the selected buttons with the color of the zone being edited.
fn accent_css(color: u8) -> String {
    let hex = match color {
        // Black would vanish on this background. White reads as "no tint".
        19 => "#f4f5f7",
        index => COLORS.get(index as usize).map(|(hex, _)| *hex).unwrap_or("#f4f5f7"),
    };
    format!(
        "button.choice.level.on {{ \
           background-color: alpha({hex}, 0.16); \
           border-color: alpha({hex}, 0.85); \
           box-shadow: 0 0 14px alpha({hex}, 0.22); }}\n\
         button.choice.on box.bar.on {{ background-color: {hex}; }}\n"
    )
}

const SECTION_WIDTH: i32 = 96;

fn control_row() -> gtk::Box {
    let row = gtk::Box::new(Orientation::Horizontal, 10);
    row.add_css_class("card");
    row.add_css_class("control-row");
    row
}

/// One line of equal-width choices. It never wraps.
fn choice_row() -> gtk::Box {
    let row = gtk::Box::new(Orientation::Horizontal, 4);
    row.set_homogeneous(true);
    row.set_hexpand(true);
    row.set_valign(Align::Center);
    row
}

fn set_on(widget: &impl WidgetExt, on: bool) {
    if on {
        widget.add_css_class("on");
    } else {
        widget.remove_css_class("on");
    }
}

#[cfg(test)]
fn brightness_name(value: u8) -> &'static str {
    BRIGHTNESS[brightness_choice(value)]
}

/// Errors are lowercase by Rust convention. The window shows them as
/// sentences.
fn sentence_case(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
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
        window {
          background-color: #0e0f13;
          color: #eef0f4;
          font-size: 13px;
        }
        scrolledwindow, viewport { background: transparent; }
        .page {
          background: transparent;
        }
        window.background {
          background-image: radial-gradient(circle at 15% 0%, rgba(120, 130, 255, 0.07), transparent 45%),
                            linear-gradient(180deg, #13151b, #0c0d10);
        }
        .title {
          font-size: 13px;
          font-weight: 800;
          letter-spacing: -0.2px;
        }
        label.status {
          font-size: 10px;
          font-weight: 600;
          color: #ffb4a8;
          background-color: rgba(255, 110, 90, 0.10);
          border: 1px solid rgba(255, 110, 90, 0.30);
          border-radius: 999px;
          padding: 2px 9px;
        }
        label.status.on {
          color: #9ff0c0;
          background-color: rgba(70, 220, 140, 0.10);
          border-color: rgba(70, 220, 140, 0.30);
        }
        .toolbar {
          background-color: rgba(255, 255, 255, 0.03);
          border: 1px solid rgba(255, 255, 255, 0.07);
          border-radius: 12px;
          padding: 1px 3px 1px 12px;
        }
        .eyebrow, .section {
          font-size: 10px;
          font-weight: 800;
          letter-spacing: 1.2px;
          color: #a9afbd;
        }
        .eyebrow { margin-right: 4px; font-size: 9px; }
        .toolbar dropdown.profile, .toolbar button.action { font-size: 12px; }
        .section { font-size: 11px; color: #dfe2e9; }
        .hint { color: #727888; font-size: 11px; }
        .card {
          background-color: rgba(255, 255, 255, 0.035);
          border: 1px solid rgba(255, 255, 255, 0.07);
          border-radius: 18px;
          padding: 10px 18px;
          box-shadow: 0 10px 30px rgba(0, 0, 0, 0.35);
        }
        .swatch-card { padding: 4px 8px; }
        .control-row { padding: 3px 8px 3px 12px; border-radius: 14px; }
        .notice {
          background-color: rgba(255, 170, 60, 0.10);
          color: #ffd9a8;
          border: 1px solid rgba(255, 170, 60, 0.35);
          border-radius: 12px;
          padding: 6px 14px;
        }
        button.action, button.primary, button.choice, button.swatch,
        button.link {
          min-height: 0;
          min-width: 0;
          box-shadow: none;
          transition: background-color 120ms ease, border-color 120ms ease,
                      box-shadow 160ms ease;
        }
        button.action {
          background: transparent;
          background-image: none;
          color: #dfe2e9;
          border: 1px solid transparent;
          border-radius: 9px;
          padding: 4px 10px;
        }
        button.action:hover {
          background: rgba(255, 255, 255, 0.07);
          background-image: none;
          border-color: rgba(255, 255, 255, 0.08);
        }
        button.action.danger:hover {
          background: rgba(255, 90, 90, 0.14);
          color: #ffb3b3;
          border-color: rgba(255, 90, 90, 0.35);
        }
        button.action:disabled { opacity: 0.35; }
        button.primary {
          background: #eef0f4;
          background-image: none;
          color: #14151a;
          border: 1px solid #eef0f4;
          border-radius: 10px;
          padding: 8px 18px;
          font-weight: 700;
        }
        button.primary:hover { background: #ffffff; background-image: none; }
        button.link {
          background: transparent;
          background-image: none;
          border: none;
          padding: 2px 4px;
          font-size: 11px;
          font-weight: 600;
          color: #f4f5f7;
          text-decoration: none;
        }
        button.link label { text-decoration: underline; }
        button.link:hover { background: transparent; background-image: none; }
        dropdown.profile {
          background: rgba(0, 0, 0, 0.25);
          background-image: none;
          color: #eef0f4;
          border: 1px solid rgba(255, 255, 255, 0.09);
          border-radius: 10px;
          padding: 0;
          min-height: 0;
        }
        dropdown.profile > button {
          background: transparent;
          background-image: none;
          color: #eef0f4;
          border: none;
          box-shadow: none;
          padding: 3px 10px;
          min-height: 0;
          font-weight: 600;
        }
        dropdown.profile > button:hover {
          background: rgba(255, 255, 255, 0.05);
          background-image: none;
        }
        button.choice {
          background: rgba(255, 255, 255, 0.04);
          background-image: none;
          color: #dfe2e9;
          border: 1px solid rgba(255, 255, 255, 0.08);
          border-radius: 9px;
          padding: 4px 6px;
          font-size: 12px;
          font-weight: 600;
        }
        button.choice:hover { background: rgba(255, 255, 255, 0.08); background-image: none; }
        button.choice.on {
          color: #ffffff;
          background-color: rgba(255, 255, 255, 0.12);
          border-color: rgba(255, 255, 255, 0.75);
          box-shadow: 0 0 12px rgba(255, 255, 255, 0.12);
        }
        box.bar {
          background-color: rgba(255, 255, 255, 0.18);
          border-radius: 2px;
          min-width: 3px;
        }
        box.bar.on { background-color: #eef0f4; }
        button.swatch {
          padding: 1px;
          border-radius: 999px;
          background: transparent;
          background-image: none;
          border: 2px solid transparent;
        }
        button.swatch:hover { border-color: rgba(255, 255, 255, 0.30); background: transparent; }
        button.swatch.on { border-color: #ffffff; }
        box.chip {
          border-radius: 999px;
          box-shadow: inset 0 0 0 1px rgba(255, 255, 255, 0.10);
        }
        flowbox.choices > flowboxchild {
          padding: 0;
          background: transparent;
          border: none;
          outline: none;
        }
        entry {
          background-color: rgba(0, 0, 0, 0.3);
          color: #eef0f4;
          border: 1px solid rgba(255, 255, 255, 0.12);
          border-radius: 10px;
          padding: 8px 10px;
          min-height: 0;
        }
        entry:focus-within { border-color: rgba(255, 255, 255, 0.35); }
        window.name-dialog { background-color: #17191f; background-image: none; }
        popover, popover contents {
          background-color: #1b1d24;
          color: #eef0f4;
        }
        dropdown.profile popover listview,
        popover.menu listview {
          background-color: #1b1d24;
          background-image: none;
          color: #eef0f4;
        }
        dropdown.profile popover listview > row,
        popover.menu listview > row {
          background-color: #1b1d24;
          background-image: none;
          color: #eef0f4;
          border-radius: 8px;
          padding: 3px 10px;
        }
        dropdown.profile popover listview > row:hover,
        popover.menu listview > row:hover,
        popover.menu listview > row.activatable:hover {
          background-color: #2c3140;
          background-image: none;
          color: #ffffff;
        }
        dropdown.profile popover listview > row:selected,
        dropdown.profile popover listview > row:selected:hover,
        popover.menu listview > row:selected,
        popover.menu listview > row:selected:hover,
        popover.menu listview > row.activatable:selected,
        popover.menu listview > row.activatable:selected:hover {
          background: #eef0f4;
          background-color: #eef0f4;
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
            "box.chip.c{index} {{ background: {hex}; background-image: none; }}\n\
             button.swatch.s{index}.on {{ box-shadow: 0 0 10px alpha({hex}, 0.55); }}\n"
        ));
    }
    // Black turns the zone off. A red slash says so.
    css.push_str(
        "box.chip.c19 { background-color: #000000; \
           background-image: linear-gradient(135deg, transparent 44%, #e51012 44%, \
           #e51012 56%, transparent 56%); }\n\
         button.swatch.s19.on { box-shadow: 0 0 10px alpha(#e51012, 0.45); }\n",
    );
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
    // Centered across, pinned to the top.
    let oy = 0.0;
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
    let off = panel.off_selected();
    let mut paints = zone_paints(panel);
    if off {
        // Unlit caps, no glow, no selection.
        for paint in &mut paints {
            paint.black = true;
        }
    }
    let selected = if off { usize::MAX } else { panel.selected_zone.get() };
    let hover = if off { None } else { panel.hover_zone.get() };

    let _ = cr.save();
    cr.translate(ox, oy);
    cr.scale(scale, scale);

    // Chassis: a dark aluminium slab with a faint top highlight.
    rounded_rect(cr, 1.0, 1.0, layout.width - 2.0, layout.height - 2.0, 18.0);
    let body = gtk::cairo::LinearGradient::new(0.0, 0.0, 0.0, layout.height);
    body.add_color_stop_rgba(0.0, 0.10, 0.11, 0.14, 1.0);
    body.add_color_stop_rgba(1.0, 0.05, 0.055, 0.07, 1.0);
    let _ = cr.set_source(&body);
    let _ = cr.fill_preserve();
    let _ = cr.save();
    cr.clip_preserve();
    // Light that leaks out from under each zone's keys.
    for (zone, paint) in paints.iter().enumerate() {
        if paint.black || paint.brightness == 0 {
            continue;
        }
        let Some((x0, y0, x1, y1)) = zone_bounds(layout, zone) else {
            continue;
        };
        let (r, g, b) = paint.rgb;
        let level = f64::from(paint.brightness) / 4.0;
        let cx = (x0 + x1) / 2.0;
        let cy = (y0 + y1) / 2.0;
        let radius = (x1 - x0).max(y1 - y0) * 0.75;
        let glow = gtk::cairo::RadialGradient::new(cx, cy, 0.0, cx, cy, radius);
        glow.add_color_stop_rgba(0.0, r, g, b, 0.16 + 0.14 * level);
        glow.add_color_stop_rgba(1.0, r, g, b, 0.0);
        let _ = cr.set_source(&glow);
        let _ = cr.paint();
    }
    let _ = cr.restore();
    cr.set_source_rgba(1.0, 1.0, 1.0, 0.08);
    cr.set_line_width(1.0);
    let _ = cr.stroke();

    for key in &layout.keys {
        let paint = &paints[key.zone as usize];
        if paint.black || paint.brightness == 0 {
            continue;
        }
        let emphasis = if hover == Some(key.zone as usize) { 0.9 } else { 0.5 };
        draw_glow(cr, key, paint, emphasis);
    }
    for key in &layout.keys {
        let zone = key.zone as usize;
        draw_key(
            cr,
            key,
            &paints[zone],
            hover == Some(zone),
            zone == selected,
        );
    }
    if off {
        draw_off_label(cr, layout);
    }
    let _ = cr.restore();
}

/// "Backlight Off" in a soft red, centered on the keyboard, on a dim pill
/// so it reads over the keys without shouting.
fn draw_off_label(cr: &gtk::cairo::Context, layout: &KeyboardLayout) {
    let text = "Backlight Off";
    cr.select_font_face(
        "Sans",
        gtk::cairo::FontSlant::Normal,
        gtk::cairo::FontWeight::Normal,
    );
    cr.set_font_size(layout.height * 0.095);
    let Ok(extents) = cr.text_extents(text) else {
        return;
    };
    let cx = layout.width / 2.0;
    let cy = layout.height / 2.0;
    let pad_x = layout.height * 0.07;
    let pad_y = layout.height * 0.045;
    let w = extents.width() + pad_x * 2.0;
    let h = extents.height() + pad_y * 2.0;
    rounded_rect(cr, cx - w / 2.0, cy - h / 2.0, w, h, h / 2.0);
    cr.set_source_rgba(0.05, 0.05, 0.06, 0.6);
    let _ = cr.fill_preserve();
    cr.set_source_rgba(0.88, 0.45, 0.45, 0.28);
    cr.set_line_width(1.0);
    let _ = cr.stroke();

    cr.move_to(
        cx - extents.width() / 2.0 - extents.x_bearing(),
        cy - extents.height() / 2.0 - extents.y_bearing(),
    );
    cr.set_source_rgba(0.90, 0.48, 0.48, 0.85);
    let _ = cr.show_text(text);
}

fn zone_bounds(layout: &KeyboardLayout, zone: usize) -> Option<(f64, f64, f64, f64)> {
    layout
        .keys
        .iter()
        .filter(|key| key.zone as usize == zone)
        .fold(None, |bounds, key| {
            let (x0, y0, x1, y1) = bounds.unwrap_or((f64::MAX, f64::MAX, f64::MIN, f64::MIN));
            Some((
                x0.min(key.x),
                y0.min(key.y),
                x1.max(key.x + key.w),
                y1.max(key.y + key.h),
            ))
        })
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

fn draw_key(
    cr: &gtk::cairo::Context,
    key: &KeyGeom,
    paint: &ZonePaint,
    hover: bool,
    selected: bool,
) {
    let (r, g, b) = if paint.black {
        (0.55, 0.57, 0.62)
    } else {
        paint.rgb
    };
    let base_tint = if paint.black {
        0.0
    } else if paint.brightness == 0 {
        0.05
    } else {
        0.12 + 0.08 * f64::from(paint.brightness) / 5.0
    };
    let tint = if hover && !paint.black {
        base_tint + 0.16
    } else {
        base_tint
    };
    let mut top = mix_rgb((0.17, 0.18, 0.22), paint.rgb, tint);
    let mut bottom = mix_rgb((0.09, 0.10, 0.12), paint.rgb, tint * 0.55);
    if hover {
        top = mix_rgb(top, (1.0, 1.0, 1.0), 0.06);
        bottom = mix_rgb(bottom, (1.0, 1.0, 1.0), 0.04);
    }

    rounded_rect(cr, key.x, key.y, key.w, key.h, 5.5);
    let gradient = gtk::cairo::LinearGradient::new(key.x, key.y, key.x, key.y + key.h);
    gradient.add_color_stop_rgba(0.0, top.0, top.1, top.2, 1.0);
    gradient.add_color_stop_rgba(1.0, bottom.0, bottom.1, bottom.2, 1.0);
    let _ = cr.set_source(&gradient);
    let _ = cr.fill_preserve();

    let edge_alpha = match (hover, paint.brightness == 0) {
        (true, true) => 0.6,
        (true, false) => 1.0,
        (false, true) => 0.38,
        (false, false) => 0.82,
    };
    cr.set_line_width(2.6);
    cr.set_source_rgba(r, g, b, edge_alpha);
    let _ = cr.stroke();

    // A thin sheen along the top of the cap.
    let sheen = gtk::cairo::LinearGradient::new(key.x, key.y, key.x, key.y + key.h * 0.45);
    sheen.add_color_stop_rgba(0.0, 1.0, 1.0, 1.0, if hover { 0.10 } else { 0.06 });
    sheen.add_color_stop_rgba(1.0, 1.0, 1.0, 1.0, 0.0);
    rounded_rect(cr, key.x + 2.0, key.y + 2.0, key.w - 4.0, key.h * 0.45, 4.0);
    let _ = cr.set_source(&sheen);
    let _ = cr.fill();

    if selected {
        let inset = 7.5;
        rounded_rect(
            cr,
            key.x + inset,
            key.y + inset,
            key.w - inset * 2.0,
            key.h - inset * 2.0,
            3.5,
        );
        cr.set_source_rgba(r, g, b, 0.95);
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
        assert_eq!(
            sentence_case("lost the backlight service. Reconnecting…"),
            "Lost the backlight service. Reconnecting…"
        );
        assert_eq!(sentence_case(""), "");
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
        let mut order = SWATCH_ORDER;
        order.sort_unstable();
        assert_eq!(order, std::array::from_fn::<usize, 20, _>(|index| index));
        assert_eq!(&SWATCH_ORDER[..2], &[19, 18]);
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
