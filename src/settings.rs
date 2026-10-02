//! The settings window, `yunta settings`. A process of its own, so the app in the background
//! stays small: it reads the status file that app keeps and writes yunta.conf, which the app
//! picks up within a moment.

use std::io;
use std::net::IpAddr;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align, Align2, Color32, CursorIcon, FontId, Layout as Flow, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, TextStyle, Vec2,
    WidgetInfo, WidgetType,
};

use crate::config::{self, Config};
use crate::crossing::{Edge, Layout, Rect as Screen};
use crate::widgets::{self, Holder, Icon, Scene, bold, card, divider, lerp_color, page_title, palette, primary, row, section, toggle_row};
use crate::{autostart, fx, icon, pair, update, wake};

const POLL: Duration = Duration::from_millis(300);
/// Assumed for the other machine's screens until it has said what they are.
const UNKNOWN: Screen = Screen { x: 0, y: 0, w: 1920, h: 1080 };

#[derive(Clone, Copy, PartialEq)]
enum Page {
    Overview,
    Arrangement,
    Keyboard,
    Pairing,
    Connection,
    About,
}

const PAGES: [(Page, Icon, &str); 6] = [
    (Page::Overview, Icon::Overview, "Overview"),
    (Page::Arrangement, Icon::Arrangement, "Arrangement"),
    (Page::Keyboard, Icon::Keyboard, "Keyboard & mouse"),
    (Page::Pairing, Icon::Pairing, "Pairing"),
    (Page::Connection, Icon::Connection, "Connection"),
    (Page::About, Icon::About, "About"),
];

/// Where things stand, as the window tells it.
#[derive(Clone, Copy, PartialEq)]
enum Link {
    NotRunning,
    NotPaired,
    /// Pairing mode is on in this window.
    Pairing,
    /// Paired, running, and the other computer is not there.
    Offline,
    Waking,
    Connected,
    Paused,
}

/// The update check, which runs on a thread of its own.
enum Update {
    Idle,
    Checking,
    Latest,
    /// The project has not published a release yet.
    NoReleases,
    Available(update::Release),
    Installing(String),
    Installed,
    Failed(String),
}

pub fn run(page: Option<&str>) -> io::Result<()> {
    let cfg = config::init()?;
    crate::log::start(&cfg.path, "window");
    // ponytail: a second window just exits, raising the first one needs a message to it
    // A window restarting itself after an update lets go of the lock as it closes.
    let wait = if std::env::var_os(crate::RESTART).is_some() { Duration::from_secs(3) } else { Duration::ZERO };
    let deadline = Instant::now() + wait;
    let _lock = loop {
        match config::lock(&cfg.path, "settings.lock")? {
            Some(lock) => break lock,
            None if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(100)),
            None => return Ok(()),
        }
    };
    let page = match page {
        Some("arrangement") => Page::Arrangement,
        Some("keyboard") => Page::Keyboard,
        Some("connection") => Page::Connection,
        Some("pairing") => Page::Pairing,
        Some("about") => Page::About,
        _ => Page::Overview,
    };
    let image = icon::image(256);
    let icon = egui::IconData { width: image.width(), height: image.height(), rgba: image.into_raw() };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Yunta Settings")
            .with_app_id("yunta")
            .with_inner_size([860.0, 620.0])
            .with_min_inner_size([720.0, 500.0])
            .with_icon(icon),
        ..Default::default()
    };
    eframe::run_native(
        "Yunta Settings",
        options,
        Box::new(move |cc| {
            widgets::install(&cc.egui_ctx);
            Ok(Box::new(App::new(cfg, page)))
        }),
    )
    .map_err(|e| io::Error::other(e.to_string()))
}

/// What the running app last wrote in the status file.
#[derive(Default)]
struct Status {
    linked: bool,
    input: String,
    waking: bool,
    /// The other computer lets this one's keyboard and mouse in.
    peer_receives: bool,
    /// An app fills the other computer's screen.
    peer_busy: bool,
    displays: Vec<Screen>,
    peer_displays: Vec<Screen>,
}

impl Status {
    fn parse(text: &str) -> Status {
        let mut s = Status { peer_receives: true, ..Status::default() };
        for (k, v) in text.lines().filter_map(|l| l.split_once('=')) {
            let v = v.trim();
            match k.trim() {
                "linked" => s.linked = v == "yes",
                "input" => s.input = v.to_string(),
                "waking" => s.waking = v == "yes",
                "peer_receives" => s.peer_receives = v == "yes",
                "peer_busy" => s.peer_busy = v == "yes",
                "displays" => s.displays = Screen::parse_list(v),
                "peer_displays" => s.peer_displays = Screen::parse_list(v),
                _ => {}
            }
        }
        s
    }
}

struct App {
    cfg: Config,
    page: Page,
    status: Status,
    running: bool,
    polled: Instant,
    name: String,
    /// While the other machine's screens are being dragged: where they are, unsnapped.
    drag: Option<Pos2>,
    /// Where on those screens they were picked up, from their top left corner.
    grab: Vec2,
    /// Where the other machine's screens were drawn last.
    peer_at: Rect,
    error: Option<String>,
    /// Pairing mode, while it is on.
    pairing: Option<Pairing>,
    /// How the last pairing mode ended.
    paired: Option<(String, bool)>,
    /// For previewing the crossing animations: started on the first preview, None inside when
    /// they cannot show here.
    fx: Option<Option<fx::Fx>>,
    /// When the page last changed, for its fade in.
    shown_at: f64,
    /// This computer's name as it is being edited, until saved.
    rename: Option<String>,
    update: Arc<Mutex<Update>>,
    /// When this window opened, and whether Yunta was quit from the tray since.
    opened: std::time::SystemTime,
    quit: bool,
    /// What the last Copy log or Open log folder said.
    log_note: Option<String>,
    /// The paired computer whose Forget is waiting for a second click.
    forgetting: Option<Vec<u8>>,
    /// When Forget was first pressed.
    forget_asked: Instant,
}

/// Pairing mode in the window: the machines it found, and what was typed for each.
struct Pairing {
    mode: pair::Mode,
    found: Vec<Found>,
    /// An address typed by hand, for a machine the beacon does not reach.
    address: String,
    code: String,
    /// The last wrong code someone typed for ours.
    note: Option<String>,
}

struct Found {
    name: String,
    addr: IpAddr,
    code: String,
    trying: bool,
    note: Option<String>,
}

impl App {
    fn new(cfg: Config, page: Page) -> App {
        let mut app = App {
            cfg,
            page,
            status: Status::default(),
            running: false,
            polled: Instant::now(),
            name: String::new(),
            drag: None,
            grab: Vec2::ZERO,
            peer_at: Rect::NOTHING,
            error: None,
            pairing: None,
            paired: None,
            fx: None,
            shown_at: 0.0,
            rename: None,
            update: Arc::new(Mutex::new(Update::Idle)),
            opened: std::time::SystemTime::now(),
            quit: false,
            log_note: None,
            forgetting: None,
            forget_asked: Instant::now(),
        };
        app.refresh();
        if app.cfg.updates {
            app.check_updates();
        }
        app
    }

    /// Asks GitHub for the latest release, on a thread so the window keeps drawing.
    fn check_updates(&self) {
        *self.update.lock().unwrap() = Update::Checking;
        let update = self.update.clone();
        std::thread::spawn(move || {
            *update.lock().unwrap() = match update::check() {
                Ok(Some(release)) => Update::Available(release),
                Ok(None) => Update::Latest,
                Err(e) if e.to_string().contains("no release published") => Update::NoReleases,
                Err(e) => Update::Failed(format!("Could not check for updates: {e}.")),
            };
        });
    }

    /// Downloads and installs the release found, then has the app and this window start again
    /// from the new version.
    fn install_update(&self) {
        let release = match std::mem::replace(&mut *self.update.lock().unwrap(), Update::Idle) {
            Update::Available(release) => release,
            other => return *self.update.lock().unwrap() = other,
        };
        *self.update.lock().unwrap() = Update::Installing(release.version.clone());
        let (update, path) = (self.update.clone(), self.cfg.path.clone());
        std::thread::spawn(move || {
            let result = update::install(&release).and_then(|()| config::set(&path, "restart", Some(&crate::now_ms().to_string())));
            *update.lock().unwrap() = match result {
                Ok(()) => Update::Installed,
                Err(e) => Update::Failed(format!("The update did not install: {e}.")),
            };
        });
    }

    /// Puts the chosen appearance on the window.
    fn apply_theme(&self, ctx: &egui::Context) {
        ctx.set_theme(match self.cfg.theme.as_str() {
            "light" => egui::ThemePreference::Light,
            "dark" => egui::ThemePreference::Dark,
            _ => egui::ThemePreference::System,
        });
    }

    fn refresh(&mut self) {
        self.polled = Instant::now();
        // Quit in the tray after this window opened: it goes too.
        self.quit = std::fs::metadata(config::quit_path(&self.cfg.path)).and_then(|m| m.modified()).is_ok_and(|t| t > self.opened);
        // ponytail: the probe holds the lock for an instant, so an app starting at that very moment says "already running"
        self.running = matches!(config::lock(&self.cfg.path, "yunta.lock"), Ok(None));
        let text =
            if self.running { std::fs::read_to_string(config::status_path(&self.cfg.path)).unwrap_or_default() } else { String::new() };
        self.status = Status::parse(&text);
        match config::load_from(&self.cfg.path) {
            Ok(cfg) => self.cfg = cfg,
            Err(e) => self.error = Some(e.to_string()),
        }
        self.name = self.cfg.display_name();
    }

    fn set(&mut self, key: &str, value: &str) {
        if let Err(e) = config::set(&self.cfg.path, key, Some(value)) {
            self.error = Some(format!("Could not save the setting: {e}"));
        }
        self.refresh();
    }

    fn peer(&self) -> String {
        self.cfg.peer_name.clone().unwrap_or_else(|| SOMEONE.into())
    }

    fn nav(&mut self, ui: &mut egui::Ui) {
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.add_space(6.0);
            let (rect, _) = ui.allocate_exact_size(Vec2::splat(26.0), Sense::hover());
            let p = palette(ui);
            ui.painter().rect_filled(rect, 7.0, p.accent);
            widgets::logo(ui.painter(), rect, Color32::WHITE);
            ui.label(bold("Yunta").size(18.0));
        });
        ui.add_space(22.0);
        for (page, which, label) in PAGES {
            if widgets::nav_item(ui, self.page == page, which, label).clicked() && self.page != page {
                self.go(ui, page);
            }
            ui.add_space(2.0);
        }
        ui.with_layout(Flow::bottom_up(Align::LEFT), |ui| {
            ui.add_space(4.0);
            ui.label(RichText::new(format!("Version {}", env!("CARGO_PKG_VERSION"))).size(11.5).color(palette(ui).weak));
            ui.add_space(4.0);
            let (text, color) = self.state(ui);
            widgets::pill(ui, text, color);
        });
    }

    fn link(&self) -> Link {
        if self.pairing.is_some() {
            Link::Pairing
        } else if self.cfg.peer_key.is_none() {
            Link::NotPaired
        } else if !self.running {
            Link::NotRunning
        } else if self.status.linked {
            if self.cfg.paused { Link::Paused } else { Link::Connected }
        } else if self.status.waking {
            Link::Waking
        } else {
            Link::Offline
        }
    }

    /// The link's state in a word, and its colour.
    fn state(&self, ui: &egui::Ui) -> (&'static str, Color32) {
        let p = palette(ui);
        match self.link() {
            Link::NotRunning => ("Not running", p.weak),
            Link::NotPaired => ("Not paired", p.weak),
            Link::Pairing => ("Waiting", p.warn),
            Link::Offline => ("Offline", p.weak),
            Link::Waking => ("Waking", p.warn),
            Link::Connected => ("Connected", p.accent),
            Link::Paused => ("Paused", p.warn),
        }
    }

    fn go(&mut self, ui: &egui::Ui, page: Page) {
        self.page = page;
        self.shown_at = ui.input(|i| i.time);
    }

    fn overview(&mut self, ui: &mut egui::Ui) {
        page_title(ui, "Overview", "One keyboard and mouse for this computer and the one next to it.");
        let available = match &*self.update.lock().unwrap() {
            Update::Available(release) => Some(release.version.clone()),
            _ => None,
        };
        if let Some(version) = available {
            let mut open = false;
            egui::Frame::new()
                .fill(palette(ui).accent.gamma_multiply(0.12))
                .corner_radius(8)
                .inner_margin(egui::Margin::symmetric(14, 8))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(format!("Yunta {version} is available.")).color(palette(ui).accent));
                        ui.with_layout(Flow::right_to_left(Align::Center), |ui| open = ui.button("See the update").clicked());
                    });
                });
            ui.add_space(12.0);
            if open {
                self.go(ui, Page::About);
            }
        }
        let peer = self.peer();
        let link = self.link();
        let (title, detail) = match link {
            Link::NotRunning => ("Yunta is not running".to_string(), "Start it to share this keyboard and mouse.".to_string()),
            Link::NotPaired => {
                ("Not paired yet".to_string(), "Pair this computer with the one next to it to share one keyboard and mouse.".to_string())
            }
            Link::Pairing => {
                ("Waiting for the other computer".to_string(), "Pairing mode is on. Turn it on on the other computer as well.".to_string())
            }
            Link::Offline => {
                (format!("{} is offline", opening(&peer)), format!("Yunta connects as soon as it runs on {peer}, on the same network."))
            }
            Link::Waking => (format!("Waking {peer}…"), format!("A wake-up went out. Yunta connects as soon as {peer} is up.")),
            Link::Connected | Link::Paused => {
                let where_ = match self.status.input.as_str() {
                    "there" => format!("The keyboard and mouse are on {peer}."),
                    "visiting" => format!("{}'s keyboard and mouse are on this computer.", opening(&peer)),
                    _ => "The keyboard and mouse are on this computer.".to_string(),
                };
                let paused = if link == Link::Paused { " Crossing at the edge is paused; the shortcut still switches." } else { "" };
                let busy = if self.status.peer_busy && link == Link::Connected {
                    format!(" An app fills {peer}'s screen, so its edge waits; the shortcut still switches.")
                } else {
                    String::new()
                };
                (format!("Connected to {peer}"), where_ + paused + &busy)
            }
        };
        let can_wake = self.cfg.peer_mac.is_some();
        card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(bold(title).size(18.0));
                ui.with_layout(Flow::right_to_left(Align::Center), |ui| match link {
                    Link::NotRunning => {
                        if primary(ui, "Start Yunta").clicked()
                            && let Err(e) = crate::exe().and_then(|exe| Command::new(exe).spawn())
                        {
                            self.error = Some(format!("Could not start Yunta: {e}"));
                        }
                    }
                    Link::NotPaired => {
                        if primary(ui, "Pair a computer").clicked() {
                            self.go(ui, Page::Pairing);
                        }
                    }
                    Link::Pairing => {
                        if ui.button("Open pairing").clicked() {
                            self.go(ui, Page::Pairing);
                        }
                    }
                    Link::Offline if can_wake => {
                        if ui.button(format!("Wake {peer}")).clicked()
                            && let Some(mac) = self.cfg.peer_mac
                            && let Err(e) = wake::send(&mac)
                        {
                            self.error = Some(format!("Could not send the wake-up: {e}"));
                        }
                    }
                    _ => {
                        let (text, color) = self.state(ui);
                        widgets::pill(ui, text, color);
                    }
                });
            });
            ui.label(RichText::new(detail).color(palette(ui).weak));
            ui.add_space(10.0);
            let scene = match link {
                Link::NotRunning => Scene::Stopped,
                Link::NotPaired => Scene::Alone,
                Link::Pairing => Scene::Searching,
                Link::Offline => Scene::Offline,
                Link::Waking => Scene::Waking,
                Link::Connected | Link::Paused => Scene::Linked(if self.status.input == "there" { Holder::There } else { Holder::Here }),
            };
            // While pairing, the space is for whichever computer comes, not the one paired before.
            let there =
                if self.cfg.peer_key.is_some() && link != Link::Pairing { opening(&peer) } else { "The other computer".to_string() };
            widgets::two_screens(ui, &self.name, &there, self.cfg.layout.edge == Edge::Left, scene);
            if !matches!(scene, Scene::Offline | Scene::Alone | Scene::Stopped) {
                ui.ctx().request_repaint_after(Duration::from_millis(33));
            }
        });
        section(ui, "Control");
        card(ui, |ui| {
            let mut send = self.cfg.send;
            let detail = if self.status.linked && !self.status.peer_receives {
                format!("{} does not allow it at the moment.", opening(&peer))
            } else {
                format!("Push through the edge, or use the shortcut, to use {peer} from here.")
            };
            if toggle_row(ui, &format!("Use this keyboard and mouse on {peer}"), &detail, &mut send) {
                self.set("send", if send { "yes" } else { "no" });
            }
            divider(ui);
            let mut receive = self.cfg.receive;
            if toggle_row(
                ui,
                &format!("Let {peer} use this computer"),
                &format!("{}'s keyboard and mouse may come over to this one.", opening(&peer)),
                &mut receive,
            ) {
                self.set("receive", if receive { "yes" } else { "no" });
            }
        });
        section(ui, "General");
        card(ui, |ui| {
            let mut fullscreen = self.cfg.fullscreen;
            let detail = "Games and films keep the edge to themselves. The shortcut still switches.";
            if toggle_row(ui, "Pause while an app is full screen", detail, &mut fullscreen) {
                self.set("fullscreen", if fullscreen { "yes" } else { "no" });
            }
            divider(ui);
            let mut paused = self.cfg.paused;
            if toggle_row(ui, "Pause crossing at the edge", "The shortcut still switches.", &mut paused) {
                self.set("paused", if paused { "yes" } else { "no" });
            }
            divider(ui);
            let mut login = autostart::enabled();
            if toggle_row(ui, "Start at login", "Yunta starts in the background when you sign in.", &mut login)
                && let Err(e) = autostart::set(login)
            {
                self.error = Some(format!("Could not change start at login: {e}"));
            }
        });
        section(ui, "Look");
        card(ui, |ui| {
            const THEMES: [&str; 3] = ["system", "light", "dark"];
            let mut chosen = THEMES.iter().position(|t| *t == self.cfg.theme).unwrap_or(0);
            let mut changed = false;
            row(ui, "Appearance", "Follows the system, or stays light or dark.", |ui| {
                changed = widgets::segmented(ui, &mut chosen, &["System", "Light", "Dark"]);
            });
            if changed {
                self.set("theme", THEMES[chosen]);
            }
        });
        section(ui, "Animation");
        card(ui, |ui| {
            let mut effects = self.cfg.effects;
            let detail = "A glow on the edge as you push against it, and a ripple where the pointer lands.";
            if toggle_row(ui, "Crossing animation", detail, &mut effects) {
                self.set("effects", if effects { "yes" } else { "no" });
            }
            ui.add_space(8.0);
            let can = effects && !self.status.displays.is_empty();
            if ui.add_enabled(can, egui::Button::new("▶  Preview on this screen")).clicked() {
                self.preview();
            }
        });
    }

    /// Plays a crossing on this computer: the glow building on the edge towards the other one,
    /// the flash as it crosses, and the ripple of landing in the middle of the screen.
    fn preview(&mut self) {
        let Some(fx) = self.fx.get_or_insert_with(fx::Fx::start).clone() else {
            self.error = Some("Animations need a compositor on X11, and none is running.".into());
            return;
        };
        let ours = bounds(&self.status.displays);
        let l = self.cfg.layout;
        let (a, b) = touching(ours, &l);
        // On the last pixel row or column inside the screen, where the pointer would be.
        let inside = |v: f32, far: bool| v as i32 - i32::from(far);
        let x = inside((a.x + b.x) / 2.0, l.edge == Edge::Right);
        let y = inside((a.y + b.y) / 2.0, l.edge == Edge::Bottom);
        let first = self.status.displays[0];
        let landing = (first.x + first.w / 2, first.y + first.h / 2);
        std::thread::spawn(move || {
            for i in 1..=24 {
                fx.push(l.edge, x, y, i as f32 / 24.0);
                std::thread::sleep(Duration::from_millis(25));
            }
            fx.leave(l.edge, x, y);
            std::thread::sleep(Duration::from_millis(320));
            fx.arrive(landing.0, landing.1);
        });
    }

    fn arrangement(&mut self, ui: &mut egui::Ui) {
        let peer = self.peer();
        page_title(ui, "Arrangement", &format!("Drag {peer} to where it stands. The pointer crosses where the two touch."));
        section(ui, "Screens");
        card(ui, |ui| {
            if self.status.displays.is_empty() {
                ui.label(RichText::new("Start Yunta to see the screens here.").color(palette(ui).weak));
                return;
            }
            self.canvas(ui, &peer);
            if self.status.peer_displays.is_empty() {
                ui.add_space(6.0);
                let note = format!("{}'s screen size shows once it connects.", opening(&peer));
                ui.label(RichText::new(note).size(12.5).color(palette(ui).weak));
            }
        });
        section(ui, "Crossing");
        card(ui, |ui| {
            row(ui, "Push-through", "How far to keep pushing past the edge before crossing.", |ui| {
                let mut r = self.cfg.resistance;
                let slider = ui.add(egui::Slider::new(&mut r, 0..=400).suffix(" px"));
                self.cfg.resistance = r;
                if slider.drag_stopped() || (slider.changed() && !slider.dragged()) {
                    self.set("resistance", &r.to_string());
                }
            });
        });
    }

    /// Both machines' screens to scale. The other machine's can be dragged; it snaps to the
    /// nearest edge of ours and keeps touching it.
    fn canvas(&mut self, ui: &mut egui::Ui, peer: &str) {
        let ours = bounds(&self.status.displays);
        let peers = if self.status.peer_displays.is_empty() { vec![UNKNOWN] } else { self.status.peer_displays.clone() };
        let theirs = bounds(&peers);
        let (resp, painter) = ui.allocate_painter(Vec2::new(ui.available_width(), 300.0), Sense::hover());
        let p = palette(ui);
        let dark = ui.visuals().dark_mode;
        // An inset surface with a faint dot grid, like a design canvas.
        painter.rect(resp.rect, 10.0, p.bg, Stroke::new(1.0, p.border), StrokeKind::Inside);
        let dot = if dark { Color32::from_white_alpha(10) } else { Color32::from_black_alpha(14) };
        let mut y = resp.rect.top() + 12.0;
        while y < resp.rect.bottom() - 6.0 {
            let mut x = resp.rect.left() + 12.0;
            while x < resp.rect.right() - 6.0 {
                painter.circle_filled(Pos2::new(x, y), 1.0, dot);
                x += 18.0;
            }
            y += 18.0;
        }
        let layout = match self.drag {
            Some(free) => snap(ours, Screen { x: free.x as i32, y: free.y as i32, ..theirs }),
            None => self.cfg.layout,
        };
        let at = place(ours, theirs, &layout);
        // At rest the view fits the two machines. While dragging it zooms out to leave room for
        // the other machine on every side of this one.
        let target = if self.drag.is_some() {
            Screen { x: ours.x - theirs.w, y: ours.y - theirs.h, w: ours.w + 2 * theirs.w, h: ours.h + 2 * theirs.h }
        } else {
            bounds(&[ours, at])
        };
        let ctx = ui.ctx().clone();
        let id = ui.id().with("view");
        let ease = |n: usize, v: i32| ctx.animate_value_with_time(id.with(n), v as f32, 0.2);
        let (x0, y0) = (ease(0, target.x), ease(1, target.y));
        let world = Rect::from_min_max(Pos2::new(x0, y0), Pos2::new(x0 + ease(2, target.w), y0 + ease(3, target.h)));
        let scale = (resp.rect.width() / world.width()).min(resp.rect.height() / world.height()) * 0.9;
        let to_screen = |p: Pos2| resp.rect.center() + (p - world.center()) * scale;
        let rect_of = |s: Screen| {
            Rect::from_min_max(to_screen(Pos2::new(s.x as f32, s.y as f32)), to_screen(Pos2::new((s.x + s.w) as f32, (s.y + s.h) as f32)))
        };
        let shift = |s: Screen| Screen { x: s.x - theirs.x + at.x, y: s.y - theirs.y + at.y, ..s };

        let dragging = self.drag.is_some();
        let peer_rect = rect_of(Screen { x: at.x, y: at.y, ..theirs });
        let hovered = ui.rect_contains_pointer(peer_rect);
        let lift = ui.ctx().animate_bool_with_time(ui.id().with("lift"), dragging || hovered, 0.12);
        let screen = |s: Screen, fill: Color32, border: Color32, raised: f32| {
            let r = rect_of(s).shrink(2.0);
            if raised > 0.0 {
                painter.rect_filled(r.translate(Vec2::new(0.0, 6.0 * raised)), 7.0, Color32::from_black_alpha((70.0 * raised) as u8));
            }
            let r = r.translate(Vec2::new(0.0, -2.0 * raised));
            painter.rect(r, 6.0, fill, Stroke::new(1.5, border), StrokeKind::Inside);
            painter.rect_filled(
                Rect::from_min_max(r.min + Vec2::splat(3.0), Pos2::new(r.max.x - 3.0, r.min.y + 10.0)),
                3.0,
                Color32::from_white_alpha(5),
            );
            if r.height() > 34.0 {
                let size = format!("{} × {}", s.w, s.h);
                painter.text(r.center_bottom() - Vec2::new(0.0, 7.0), Align2::CENTER_BOTTOM, size, FontId::proportional(10.5), p.weak);
            }
        };
        for &s in &self.status.displays {
            screen(s, p.control, p.frame(), 0.0);
        }
        let peer_fill = lerp_color(p.card, p.accent, if dark { 0.22 } else { 0.14 } + 0.08 * lift);
        for &s in &peers {
            screen(shift(s), peer_fill, p.accent, lift);
        }
        let name = |r: Rect, text: &str, color: Color32| {
            painter.text(r.center() - Vec2::new(0.0, 4.0), Align2::CENTER_CENTER, text, FontId::new(13.5, widgets::semibold()), color);
        };
        name(rect_of(ours), &self.name, p.text);
        name(peer_rect.translate(Vec2::new(0.0, -2.0 * lift)), peer, p.text);

        // Where the pointer crosses, breathing gently.
        let (a, b) = touching(ours, &layout);
        let breathe = 0.7 + 0.3 * (ui.input(|i| i.time) as f32 * 2.4).sin();
        if layout.corner {
            // A corner: one point, crossed with a diagonal push.
            painter.circle_filled(to_screen(a), 12.0, p.accent.gamma_multiply(0.22 * breathe));
            painter.circle_filled(to_screen(a), 5.0, p.accent);
        } else {
            painter.line_segment([to_screen(a), to_screen(b)], Stroke::new(10.0, p.accent.gamma_multiply(0.18 * breathe)));
            painter.line_segment([to_screen(a), to_screen(b)], Stroke::new(3.0, p.accent));
        }
        ui.ctx().request_repaint_after(Duration::from_millis(40));
        let hint = match (dragging, layout.corner) {
            (true, true) => "Release to cross at this corner",
            (true, false) => "Release to place",
            (false, true) => "Crosses at the corner, with a diagonal push",
            (false, false) => "Drag to rearrange, or diagonally for a corner",
        };
        painter.text(resp.rect.right_bottom() - Vec2::new(12.0, 10.0), Align2::RIGHT_BOTTOM, hint, FontId::proportional(11.5), p.weak);

        self.peer_at = peer_rect;
        let handle = ui.interact(peer_rect, ui.id().with("peer-screens"), Sense::drag()).on_hover_cursor(CursorIcon::Grab);
        handle.widget_info(|| WidgetInfo::labeled(WidgetType::Other, true, format!("{peer}'s screens, drag to move")));
        // Follows the pointer itself rather than adding up its moves, so the zoom cannot make
        // the screens drift away from under it.
        let to_world = |p: Pos2| world.center() + (p - resp.rect.center()) / scale;
        if let Some(p) = handle.interact_pointer_pos() {
            if handle.drag_started() {
                self.grab = to_world(p) - Pos2::new(at.x as f32, at.y as f32);
            }
            if handle.dragged() {
                ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
                self.drag = Some(to_world(p) - self.grab);
            }
        }
        if handle.drag_stopped() {
            self.drag = None;
            let layout = Layout { stamp: crate::now_ms(), ..layout };
            if let Err(e) = config::save_layout(&self.cfg.path, &layout) {
                self.error = Some(format!("Could not save the arrangement: {e}"));
            }
            self.refresh();
        }
    }

    /// Takes in what pairing mode has heard since the last frame.
    fn pump(&mut self) {
        let Some(p) = &mut self.pairing else { return };
        let mut done = None;
        loop {
            match p.mode.next(Duration::ZERO) {
                Ok(None) => break,
                Ok(Some(pair::Event::Found(name, addr))) => {
                    if !p.found.iter().any(|f| f.addr == addr) {
                        p.found.push(Found { name, addr, code: String::new(), trying: false, note: None });
                    }
                }
                Ok(Some(pair::Event::Paired(paired))) => {
                    done = Some(Ok(paired));
                    break;
                }
                Ok(Some(pair::Event::WrongCode(addr, n))) => p.note = Some(format!("{addr} typed a wrong code ({n} of 3).")),
                Ok(Some(pair::Event::Refused(addr) | pair::Event::Failed(addr, _))) if !p.found.iter().any(|f| f.addr == addr) => {}
                Ok(Some(pair::Event::Refused(addr))) => {
                    let f = p.found.iter_mut().find(|f| f.addr == addr).expect("checked");
                    (f.trying, f.note) = (false, Some(format!("That is not the code {} shows.", f.name)));
                }
                Ok(Some(pair::Event::Failed(addr, e))) => {
                    let f = p.found.iter_mut().find(|f| f.addr == addr).expect("checked");
                    (f.trying, f.note) = (false, Some(format!("Could not reach it: {e}")));
                }
                Err(e) => {
                    done = Some(Err(e));
                    break;
                }
            }
        }
        match done {
            None => {}
            Some(Err(e)) => {
                self.pairing = None;
                self.paired = Some((capitalize(&e.to_string()) + ".", false));
            }
            Some(Ok(paired)) => {
                // Over now, which closes its ports.
                self.pairing = None;
                let path = self.cfg.path.clone();
                let saved = config::use_device(&path, &self.cfg, paired.device());
                self.paired = Some(match saved {
                    Ok(()) => (format!("Paired with {}.", paired.name), true),
                    Err(e) => (format!("Paired with {}, but could not save it: {e}", paired.name), false),
                });
                // A running app restarts itself with the new key; otherwise start it.
                if !self.running
                    && let Err(e) = crate::exe().and_then(|exe| Command::new(exe).spawn())
                {
                    self.error = Some(format!("Could not start Yunta: {e}"));
                }
                self.refresh();
            }
        }
    }

    /// Every computer paired with: when each last connected, which is in use, and a way to switch
    /// to another or forget one.
    fn devices(&mut self, ui: &mut egui::Ui) {
        if self.cfg.devices.is_empty() {
            return;
        }
        enum Act {
            Use(config::Device),
            Ask(Vec<u8>),
            Cancel,
            Forget(config::Device),
        }
        let mut act = None;
        let (in_use, asking, asked_at) = (self.cfg.peer_key.clone(), self.forgetting.clone(), self.forget_asked);
        // The one in use first, then the most recently connected.
        let mut devices = self.cfg.devices.clone();
        devices.sort_by_key(|d| (in_use.as_ref() != Some(&d.key), std::cmp::Reverse(d.seen)));
        section(ui, "Paired computers");
        card(ui, |ui| {
            let p = palette(ui);
            for (i, d) in devices.iter().enumerate() {
                let current = in_use.as_ref() == Some(&d.key);
                let confirming = asking.as_ref() == Some(&d.key);
                ui.horizontal(|ui| {
                    widgets::avatar(ui, &d.name);
                    ui.vertical(|ui| {
                        ui.horizontal(|ui| {
                            ui.label(bold(&d.name));
                            if current {
                                widgets::pill(ui, "In use", p.accent);
                            }
                        });
                        let seen = if d.seen == 0 { "Never connected".to_string() } else { format!("Last connected {}", ago(d.seen)) };
                        ui.label(RichText::new(format!("{seen} · key {}", fingerprint(&d.key))).size(12.0).color(p.weak));
                    });
                    ui.with_layout(Flow::right_to_left(Align::Center), |ui| {
                        if confirming {
                            // Cancel takes the place Forget was in, and the real Forget waits
                            // half a second, so a double-click cannot forget by accident.
                            if ui.button("Cancel").clicked() {
                                act = Some(Act::Cancel);
                            }
                            let ready = asked_at.elapsed() >= Duration::from_millis(500);
                            let forget = egui::Button::new(RichText::new("Forget for good").color(Color32::WHITE)).fill(p.danger);
                            if ui.add_enabled(ready, forget).clicked() {
                                act = Some(Act::Forget(d.clone()));
                            }
                            if !ready {
                                ui.ctx().request_repaint_after(Duration::from_millis(100));
                            }
                        } else {
                            if ui.button("Forget").clicked() {
                                act = Some(Act::Ask(d.key.clone()));
                            }
                            if !current && ui.button("Use").clicked() {
                                act = Some(Act::Use(d.clone()));
                            }
                        }
                    });
                });
                if confirming {
                    let note = format!("{} will no longer be able to connect. Forget this computer on {} too.", d.name, d.name);
                    ui.label(RichText::new(note).size(12.0).color(p.danger));
                }
                if i + 1 < devices.len() {
                    divider(ui);
                }
            }
        });
        let path = self.cfg.path.clone();
        let done = match act {
            None => return,
            Some(Act::Ask(key)) => {
                self.forget_asked = Instant::now();
                return self.forgetting = Some(key);
            }
            Some(Act::Cancel) => return self.forgetting = None,
            Some(Act::Use(d)) => config::use_device(&path, &self.cfg, d.clone()).map(|()| format!("Now using {}.", d.name)),
            Some(Act::Forget(d)) => config::forget_device(&path, &self.cfg, &d.key).map(|()| format!("Forgot {}.", d.name)),
        };
        self.forgetting = None;
        self.paired = Some(match done {
            Ok(note) => (note, true),
            Err(e) => (format!("Could not change the paired computers: {e}"), false),
        });
        self.refresh();
    }

    fn pairing(&mut self, ui: &mut egui::Ui) {
        page_title(ui, "Pairing", "Pair this computer with the one it shares a keyboard and mouse with.");
        if let Some((note, ok)) = &self.paired {
            widgets::banner(ui, note, *ok);
        }
        let Some(p) = &mut self.pairing else {
            card(ui, |ui| {
                let current = match &self.cfg.peer_name {
                    Some(name) if self.cfg.peer_key.is_some() => format!("Paired with {name}"),
                    _ if self.cfg.peer_key.is_some() => "Paired".to_string(),
                    _ => "Not paired yet".to_string(),
                };
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(bold(current).size(16.0));
                        let detail = "Turn pairing mode on here and on the other computer, on the same network. \
                                      It turns itself off after two minutes, or after three wrong codes.";
                        ui.label(RichText::new(detail).size(12.5).color(palette(ui).weak));
                    });
                });
                ui.add_space(10.0);
                if primary(ui, "Start pairing mode").clicked() {
                    match pair::Mode::start(&self.cfg.public, &self.cfg.display_name()) {
                        Ok(mode) => {
                            self.paired = None;
                            self.pairing = Some(Pairing { mode, found: vec![], address: String::new(), code: String::new(), note: None });
                        }
                        Err(e) => self.paired = Some((format!("Could not start pairing mode: {e}"), false)),
                    }
                }
            });
            self.devices(ui);
            return;
        };
        let mut stop = false;
        section(ui, "This computer");
        card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.label(bold(&p.mode.name).size(16.0));
                    ui.label(RichText::new("Type this code on the other computer.").size(12.5).color(palette(ui).weak));
                });
                ui.with_layout(Flow::right_to_left(Align::Min), |ui| stop = ui.button("Stop").clicked());
            });
            ui.add_space(12.0);
            widgets::code_digits(ui, &p.mode.code);
            ui.add_space(14.0);
            let left = p.mode.left();
            widgets::progress(ui, left.as_secs_f32() / 120.0);
            ui.add_space(4.0);
            let secs = left.as_secs();
            ui.label(RichText::new(format!("Pairing mode ends in {}:{:02}", secs / 60, secs % 60)).size(12.0).color(palette(ui).weak));
            if let Some(note) = &p.note {
                ui.add_space(4.0);
                ui.colored_label(palette(ui).warn, note);
            }
        });
        section(ui, "Computers in pairing mode");
        card(ui, |ui| {
            if p.found.is_empty() {
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new().color(palette(ui).accent));
                    ui.label(RichText::new("Looking on this network…").color(palette(ui).weak));
                });
                ui.add_space(4.0);
            }
            for f in &mut p.found {
                ui.horizontal(|ui| {
                    widgets::avatar(ui, &f.name);
                    ui.vertical(|ui| {
                        ui.label(bold(&f.name));
                        ui.label(RichText::new(f.addr.to_string()).size(12.0).color(palette(ui).weak));
                    });
                    ui.with_layout(Flow::right_to_left(Align::Center), |ui| {
                        if code_entry(ui, &mut f.code, f.trying, &f.name) {
                            (f.trying, f.note) = (true, None);
                            p.mode.join(f.addr, &f.code);
                        }
                    });
                });
                if let Some(note) = &f.note {
                    ui.colored_label(palette(ui).danger, note);
                }
                divider(ui);
            }
            ui.horizontal(|ui| {
                ui.label(RichText::new("Not listed?").color(palette(ui).weak));
                ui.add(egui::TextEdit::singleline(&mut p.address).hint_text("Its address").desired_width(150.0));
                ui.with_layout(Flow::right_to_left(Align::Center), |ui| {
                    if code_entry(ui, &mut p.code, false, "the computer at that address") {
                        match p.address.trim().parse::<IpAddr>() {
                            Ok(addr) => {
                                p.found.retain(|f| f.addr != addr);
                                let code = std::mem::take(&mut p.code);
                                p.mode.join(addr, &code);
                                p.found.push(Found { name: addr.to_string(), addr, code, trying: true, note: None });
                            }
                            Err(_) => p.note = Some("Type an address like 192.168.1.20.".into()),
                        }
                    }
                });
            });
        });
        if stop {
            self.pairing = None;
        }
    }

    fn keyboard(&mut self, ui: &mut egui::Ui) {
        let peer = self.peer();
        page_title(ui, "Keyboard & mouse", "How switching works, and how the two computers' keys and pointers behave.");
        section(ui, "Switching");
        card(ui, |ui| {
            let key = key_name(self.cfg.hotkey);
            let mut chosen = usize::from(self.cfg.hold);
            let mut changed = false;
            let detail = if self.cfg.hold {
                format!("Hold {key} on its own for a moment to switch, without moving the pointer.")
            } else {
                format!("Tap {key} twice, quickly, to switch without moving the pointer.")
            };
            row(ui, &format!("Shortcut: {key}"), &detail, |ui| changed = widgets::segmented(ui, &mut chosen, &["Double-tap", "Hold"]));
            if changed {
                self.set("trigger", if chosen == 1 { "hold" } else { "double" });
            }
        });
        section(ui, &format!("{}'s pointer here", opening(&peer)));
        card(ui, |ui| {
            for (key, title, detail) in [
                ("pointer_speed", "Pointer speed", "How far the pointer goes here for each move of the other computer's mouse."),
                ("scroll_speed", "Scrolling speed", "How far a page scrolls here for each turn of its wheel."),
            ] {
                let mut v = if key == "pointer_speed" { self.cfg.pointer_speed } else { self.cfg.scroll_speed };
                let mut save = false;
                row(ui, title, detail, |ui| {
                    let slider = ui.add(egui::Slider::new(&mut v, 0.25..=3.0).suffix("×").fixed_decimals(2).logarithmic(true));
                    save = slider.drag_stopped() || (slider.changed() && !slider.dragged());
                });
                *(if key == "pointer_speed" { &mut self.cfg.pointer_speed } else { &mut self.cfg.scroll_speed }) = v;
                if save {
                    self.set(key, &format!("{v:.2}"));
                }
                if key == "pointer_speed" {
                    divider(ui);
                }
            }
        });
        section(ui, "Keys");
        card(ui, |ui| {
            let mut chosen = usize::from(self.cfg.swap_modifiers);
            let mut changed = false;
            let detail = "Swap turns a Mac keyboard's ⌘ into Ctrl on the other computer, and Ctrl into the Windows key.";
            row(ui, "Ctrl and ⌘ / Windows key", detail, |ui| changed = widgets::segmented(ui, &mut chosen, &["As they are", "Swapped"]));
            if changed {
                self.set("swap_modifiers", if chosen == 1 { "yes" } else { "no" });
            }
        });
        section(ui, "Stay on this computer");
        card(ui, |ui| {
            let groups = [
                ("volume", "Volume keys", "Mute, louder and quieter work these speakers."),
                ("media", "Media keys", "Play, pause, next and previous control what plays here."),
                ("side_buttons", "Mouse back and forward", "The side buttons go back and forward here."),
                ("print_screen", "Print Screen", "Screenshots are taken of this computer."),
            ];
            for (i, (group, title, detail)) in groups.into_iter().enumerate() {
                let mut on = self.cfg.keep.iter().any(|g| g == group);
                if toggle_row(ui, title, detail, &mut on) {
                    let mut keep: Vec<&str> =
                        config::KEEP.iter().copied().filter(|g| *g != group && self.cfg.keep.iter().any(|k| k == g)).collect();
                    if on {
                        keep.push(group);
                    }
                    self.set("keep", &keep.join(", "));
                }
                if i + 1 < groups.len() {
                    divider(ui);
                }
            }
            ui.add_space(4.0);
            ui.label(
                RichText::new("While this keyboard and mouse are on the other computer, these act here instead.")
                    .size(12.0)
                    .color(palette(ui).weak),
            );
        });
        if cfg!(target_os = "linux") {
            section(ui, "Gestures");
            card(ui, |ui| {
                let mut on = self.cfg.gestures;
                let detail = format!(
                    "Three and four finger swipes on this touchpad do what they do on a Windows touchpad on {peer}: up for Task View, down for the desktop, sideways to switch apps or desktops."
                );
                if toggle_row(ui, "Touchpad swipes", &detail, &mut on) {
                    self.set("gestures", if on { "yes" } else { "no" });
                }
            });
        }
    }

    fn about(&mut self, ui: &mut egui::Ui) {
        page_title(ui, "About", "Version, updates, and what Yunta is.");
        card(ui, |ui| {
            ui.horizontal(|ui| {
                let (rect, _) = ui.allocate_exact_size(Vec2::splat(48.0), Sense::hover());
                ui.painter().rect_filled(rect, 12.0, palette(ui).accent);
                widgets::logo(ui.painter(), rect, Color32::WHITE);
                ui.vertical(|ui| {
                    ui.label(bold("Yunta").size(20.0));
                    ui.label(RichText::new(format!("Version {}", env!("CARGO_PKG_VERSION"))).color(palette(ui).weak));
                });
            });
            ui.add_space(8.0);
            ui.label(
                RichText::new(
                    "One keyboard and mouse for two computers on the same network. No account, no server, nothing leaves the network.",
                )
                .color(palette(ui).weak),
            );
        });
        section(ui, "Troubleshooting");
        card(ui, |ui| {
            let detail = "Yunta writes what it does to a log on each computer. If something goes wrong, copy it and send it along.";
            ui.label(RichText::new(detail).size(12.5).color(palette(ui).weak));
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.button("Copy log").clicked() {
                    let text = crate::log::tail(&self.cfg.path, 400);
                    self.log_note = Some(match arboard::Clipboard::new().and_then(|mut c| c.set_text(text)) {
                        Ok(()) => "Copied the last 400 lines.".into(),
                        Err(e) => format!("Could not copy it: {e}"),
                    });
                }
                if ui.button("Open log folder").clicked() {
                    let dir = self.cfg.path.parent().map(|d| d.to_path_buf()).unwrap_or_default();
                    let opener = if cfg!(windows) { "explorer" } else { "xdg-open" };
                    if let Err(e) = Command::new(opener).arg(&dir).spawn() {
                        self.log_note = Some(format!("Could not open {}: {e}", dir.display()));
                    }
                }
                if let Some(note) = &self.log_note {
                    ui.label(RichText::new(note).size(12.5).color(palette(ui).weak));
                }
            });
        });
        section(ui, "Updates");
        let mut check = false;
        let mut install = false;
        let mut restart = false;
        card(ui, |ui| {
            let p = palette(ui);
            match &*self.update.lock().unwrap() {
                Update::Idle => {
                    ui.label("Not checked yet.");
                }
                Update::Checking => {
                    ui.horizontal(|ui| {
                        ui.add(egui::Spinner::new().color(p.accent));
                        ui.label("Checking for updates…");
                    });
                    ui.ctx().request_repaint_after(Duration::from_millis(100));
                }
                Update::Latest => {
                    ui.label(RichText::new("✓  You have the latest version.").color(p.accent));
                }
                Update::NoReleases => {
                    ui.label(RichText::new(format!("No release has been published at github.com/{} yet.", update::REPO)).color(p.weak));
                }
                Update::Available(release) => {
                    ui.horizontal(|ui| {
                        ui.vertical(|ui| {
                            ui.label(bold(format!("Yunta {} is available", release.version)).size(16.0));
                            let how = if cfg!(windows) {
                                "It replaces this copy and starts again."
                            } else {
                                "It asks for your password to install, then starts again."
                            };
                            ui.label(RichText::new(how).size(12.5).color(p.weak));
                        });
                        ui.with_layout(Flow::right_to_left(Align::Min), |ui| install = primary(ui, "Update now").clicked());
                    });
                }
                Update::Installing(version) => {
                    ui.horizontal(|ui| {
                        ui.add(egui::Spinner::new().color(p.accent));
                        ui.label(format!("Installing Yunta {version}…"));
                    });
                    ui.ctx().request_repaint_after(Duration::from_millis(100));
                }
                Update::Installed => {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("✓  Updated. Yunta has started again with the new version.").color(p.accent));
                        ui.with_layout(Flow::right_to_left(Align::Center), |ui| restart = primary(ui, "Reopen this window").clicked());
                    });
                }
                Update::Failed(why) => {
                    ui.colored_label(p.danger, why);
                }
            }
            divider(ui);
            let mut auto = self.cfg.updates;
            let detail = "Asks GitHub for the latest release when this window opens. Releases are signed, and nothing installs unless the signature is the project's.";
            if toggle_row(ui, "Check for updates automatically", detail, &mut auto) {
                self.set("updates", if auto { "yes" } else { "no" });
            }
            ui.add_space(6.0);
            let busy = matches!(&*self.update.lock().unwrap(), Update::Checking | Update::Installing(_));
            check = ui.add_enabled(!busy, egui::Button::new("Check now")).clicked();
        });
        if check {
            self.check_updates();
        }
        if install {
            self.install_update();
        }
        if restart {
            match crate::exe().and_then(|exe| Command::new(exe).args(["settings", "about"]).env(crate::RESTART, "1").spawn()) {
                Ok(_) => ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close),
                Err(e) => self.error = Some(format!("Could not reopen the window: {e}")),
            }
        }
    }

    fn connection(&mut self, ui: &mut egui::Ui) {
        page_title(ui, "Connection", "The two computers talk directly over the local network, encrypted end to end.");
        let peer = self.peer();
        let rows = |ui: &mut egui::Ui, id: &str, items: Vec<(&str, String)>| {
            egui::Grid::new(id).num_columns(2).spacing([28.0, 12.0]).min_col_width(110.0).show(ui, |ui| {
                for (label, value) in items {
                    ui.label(RichText::new(label).color(palette(ui).weak));
                    ui.add(egui::Label::new(value).selectable(true).truncate());
                    ui.end_row();
                }
            });
        };
        section(ui, "This computer");
        card(ui, |ui| {
            let mut save = false;
            let detail = "What the other computer and pairing mode call this one.";
            row(ui, "Name", detail, |ui| {
                let editing = self.rename.is_some();
                let name = self.rename.get_or_insert_with(|| self.name.clone());
                let changed = config::clean_name(name) != self.name && !config::clean_name(name).is_empty();
                save = ui.add_enabled(changed, egui::Button::new("Save")).clicked();
                let field = ui.add(egui::TextEdit::singleline(name).desired_width(220.0).char_limit(48));
                save |= changed && field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if !editing && !field.has_focus() && !changed {
                    self.rename = None;
                }
            });
            if save && let Some(name) = self.rename.take() {
                self.set("name", &config::clean_name(&name));
            }
            divider(ui);
            rows(ui, "here", vec![("Key", fingerprint(&self.cfg.public))]);
        });
        section(ui, "The other computer");
        card(ui, |ui| {
            let how = match &self.cfg.peer {
                Some(addr) => format!("This computer connects to {addr}, port {}", self.cfg.port),
                None => format!("Waits for {peer} to connect, port {}", self.cfg.port),
            };
            match &self.cfg.peer_key {
                Some(key) => rows(ui, "there", vec![("Name", opening(&peer)), ("Key", fingerprint(key)), ("Link", how)]),
                None => rows(ui, "there", vec![("Paired with", "Nothing yet".into())]),
            }
        });
        section(ui, "Wake-on-LAN");
        card(ui, |ui| {
            let mut on = self.cfg.wake;
            let detail = format!("Pushing through the edge or using the shortcut while {peer} sleeps wakes it first.");
            if toggle_row(ui, &format!("Wake {peer} when switching to it"), &detail, &mut on) {
                self.set("wake", if on { "yes" } else { "no" });
            }
            ui.add_space(4.0);
            let mac = match self.cfg.peer_mac {
                Some(mac) => format!("Its hardware address, {}, was learnt when they last connected.", config::mac_text(&mac)),
                None => "Its hardware address is learnt the first time they connect.".to_string(),
            };
            ui.label(RichText::new(mac + " Wake-on-LAN also has to be on in its network settings.").size(12.0).color(palette(ui).weak));
        });
        section(ui, "Security");
        card(ui, |ui| {
            ui.label(bold("Paired keys, fresh keys for every connection").size(14.5));
            let detail = "Each computer accepts only the one key it was paired with. Every connection gets fresh keys \
                          (Noise_KK, X25519, ChaCha20-Poly1305), and only addresses on the local network may connect.";
            ui.label(RichText::new(detail).size(12.5).color(palette(ui).weak));
            divider(ui);
            rows(ui, "file", vec![("Settings file", home_relative(&self.cfg.path))]);
        });
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.pump();
        if self.polled.elapsed() >= POLL && self.drag.is_none() {
            self.refresh();
        }
        self.apply_theme(ui.ctx());
        if self.quit {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }
        ui.ctx().request_repaint_after(POLL);
        let p = palette(ui);
        egui::Panel::left("nav")
            .resizable(false)
            .exact_size(212.0)
            .show_separator_line(false)
            .frame(egui::Frame::new().fill(p.sidebar).inner_margin(14.0))
            .show(ui, |ui| {
                let edge = ui.max_rect().right() + 14.0;
                ui.painter().vline(edge, ui.max_rect().y_range().expand(14.0), Stroke::new(1.0, p.border));
                self.nav(ui);
            });
        egui::CentralPanel::default().frame(egui::Frame::new().fill(p.bg).inner_margin(egui::Margin::symmetric(36, 30))).show(ui, |ui| {
            if let Some(e) = self.error.clone() {
                ui.horizontal(|ui| {
                    ui.colored_label(p.danger, e);
                    if ui.small_button("Dismiss").clicked() {
                        self.error = None;
                    }
                });
                ui.add_space(8.0);
            }
            // Each page fades in and settles a few points up, as it opens.
            let t = ((ui.input(|i| i.time) - self.shown_at) / 0.22).clamp(0.0, 1.0) as f32;
            let t = 1.0 - (1.0 - t).powi(3);
            if t < 1.0 {
                ui.ctx().request_repaint();
            }
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.multiply_opacity(t);
                ui.add_space(10.0 * (1.0 - t));
                ui.set_max_width(ui.available_width().min(760.0));
                match self.page {
                    Page::Overview => self.overview(ui),
                    Page::Arrangement => self.arrangement(ui),
                    Page::Keyboard => self.keyboard(ui),
                    Page::Pairing => self.pairing(ui),
                    Page::Connection => self.connection(ui),
                    Page::About => self.about(ui),
                }
            });
        });
    }
}

/// A six-digit code field and its Pair button. True when Pair was pressed, or Enter with six
/// digits in.
fn code_entry(ui: &mut egui::Ui, code: &mut String, trying: bool, whose: &str) -> bool {
    let ready = code.len() == 6 && !trying;
    let pressed = ui.add_enabled(ready, egui::Button::new(if trying { "Pairing…" } else { "Pair" })).clicked();
    let field = ui.add(
        egui::TextEdit::singleline(code)
            .hint_text("Code")
            .char_limit(6)
            .desired_width(80.0)
            .font(TextStyle::Monospace)
            .interactive(!trying),
    );
    field.widget_info(|| WidgetInfo::labeled(WidgetType::TextEdit, !trying, format!("Code shown on {whose}")));
    code.retain(|c| c.is_ascii_digit());
    pressed || (ready && field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)))
}

/// The smallest rectangle around all of them.
fn bounds(screens: &[Screen]) -> Screen {
    let x0 = screens.iter().map(|s| s.x).min().unwrap_or(0);
    let y0 = screens.iter().map(|s| s.y).min().unwrap_or(0);
    let x1 = screens.iter().map(|s| s.x + s.w).max().unwrap_or(1);
    let y1 = screens.iter().map(|s| s.y + s.h).max().unwrap_or(1);
    Screen { x: x0, y: y0, w: x1 - x0, h: y1 - y0 }
}

fn frac(v: u16) -> f64 {
    f64::from(v) / 65535.0
}

fn to_u16(f: f64) -> u16 {
    (f.clamp(0.0, 1.0) * 65535.0).round() as u16
}

/// Where the other machine's screens (`theirs`, their size) sit beside ours under `l`.
fn place(ours: Screen, theirs: Screen, l: &Layout) -> Screen {
    let (a0, b0) = (frac(l.ours.0), frac(l.theirs.0));
    let along = |start: i32, our_len: i32, their_len: i32| start + (a0 * f64::from(our_len) - b0 * f64::from(their_len)).round() as i32;
    let (x, y) = match l.edge {
        Edge::Right => (ours.x + ours.w, along(ours.y, ours.h, theirs.h)),
        Edge::Left => (ours.x - theirs.w, along(ours.y, ours.h, theirs.h)),
        Edge::Bottom => (along(ours.x, ours.w, theirs.w), ours.y + ours.h),
        Edge::Top => (along(ours.x, ours.w, theirs.w), ours.y - theirs.h),
    };
    Screen { x, y, ..theirs }
}

/// The arrangement for the other machine's screens dropped at `theirs`: the nearest edge of
/// ours, kept touching it by at least an eighth of the shorter side. Within 3% of lining up
/// with our ends or middle, it lines up.
fn snap(ours: Screen, theirs: Screen) -> Layout {
    let centre = |s: Screen| (f64::from(s.x) + f64::from(s.w) / 2.0, f64::from(s.y) + f64::from(s.h) / 2.0);
    let ((ox, oy), (tx, ty)) = (centre(ours), centre(theirs));
    let dx = (tx - ox) / f64::from(ours.w + theirs.w);
    let dy = (ty - oy) / f64::from(ours.h + theirs.h);
    // Dropped clear of both our sides at once: they meet at a corner.
    let overlap = |a: i32, a_len: i32, b: i32, b_len: i32| (a + a_len).min(b + b_len) - a.max(b);
    if overlap(ours.x, ours.w, theirs.x, theirs.w) <= 0 && overlap(ours.y, ours.h, theirs.y, theirs.h) <= 0 {
        let edge = if dx >= 0.0 { Edge::Right } else { Edge::Left };
        let (ours, theirs) = if dy < 0.0 { (0, u16::MAX) } else { (u16::MAX, 0) };
        return Layout { edge, ours: (ours, ours), theirs: (theirs, theirs), stamp: 0, corner: true };
    }
    let edge = match (dx.abs() >= dy.abs(), dx >= 0.0, dy >= 0.0) {
        (true, true, _) => Edge::Right,
        (true, false, _) => Edge::Left,
        (false, _, true) => Edge::Bottom,
        (false, _, false) => Edge::Top,
    };
    let (start, len, their_start, their_len) =
        if matches!(edge, Edge::Left | Edge::Right) { (ours.y, ours.h, theirs.y, theirs.h) } else { (ours.x, ours.w, theirs.x, theirs.w) };
    let min = len.min(their_len) / 8;
    let mut t = their_start.clamp(start - their_len + min, start + len - min);
    let near = (len.max(their_len) * 3 / 100).max(1);
    for aligned in [start, start + len - their_len, start + (len - their_len) / 2] {
        if (t - aligned).abs() <= near {
            t = aligned;
            break;
        }
    }
    let (lo, hi) = (start.max(t), (start + len).min(t + their_len));
    let f = |v: i32, from: i32, n: i32| to_u16(f64::from(v - from) / f64::from(n));
    Layout {
        edge,
        ours: (f(lo, start, len), f(hi, start, len)),
        theirs: (f(lo, t, their_len), f(hi, t, their_len)),
        stamp: 0,
        corner: false,
    }
}

/// The stretch of our edge that touches the other machine, as two points.
fn touching(ours: Screen, l: &Layout) -> (Pos2, Pos2) {
    let (a0, a1) = (frac(l.ours.0), frac(l.ours.1));
    let (x0, y0, x1, y1) = (ours.x as f32, ours.y as f32, (ours.x + ours.w) as f32, (ours.y + ours.h) as f32);
    let at = |from: f32, len: f32, f: f64| from + len * f as f32;
    match l.edge {
        Edge::Right | Edge::Left => {
            let x = if l.edge == Edge::Right { x1 } else { x0 };
            (Pos2::new(x, at(y0, y1 - y0, a0)), Pos2::new(x, at(y0, y1 - y0, a1)))
        }
        Edge::Top | Edge::Bottom => {
            let y = if l.edge == Edge::Bottom { y1 } else { y0 };
            (Pos2::new(at(x0, x1 - x0, a0), y), Pos2::new(at(x0, x1 - x0, a1), y))
        }
    }
}

/// The first eight bytes of a key, in groups of four hex digits: enough to compare by eye.
fn fingerprint(key: &[u8]) -> String {
    let hex = config::hex(&key[..key.len().min(8)]);
    hex.as_bytes().chunks(4).map(|c| String::from_utf8_lossy(c).into_owned()).collect::<Vec<_>>().join(" ")
}

/// `~/…` for a path under the home folder, which is shorter and says the same.
fn home_relative(path: &std::path::Path) -> String {
    match std::env::var_os("HOME").and_then(|h| path.strip_prefix(h).ok().map(|p| format!("~/{}", p.display()))) {
        Some(short) => short,
        None => path.display().to_string(),
    }
}

fn key_name(hid: u16) -> String {
    match hid {
        0xE0 => "Left Ctrl".into(),
        0xE1 => "Left Shift".into(),
        0xE2 => "Left Alt".into(),
        0xE4 => "Right Ctrl".into(),
        0xE5 => "Right Shift".into(),
        0xE6 => "Right Alt".into(),
        0x39 => "Caps Lock".into(),
        0x48 => "Pause".into(),
        0x68..=0x73 => format!("F{}", hid - 0x68 + 13),
        _ => format!("key {hid:#04x}"),
    }
}

/// How long ago `ms` (Unix milliseconds) was, in words.
fn ago(ms: u64) -> String {
    let secs = crate::now_ms().saturating_sub(ms) / 1000;
    let (n, unit) = match secs {
        0..60 => return "just now".into(),
        60..3600 => (secs / 60, "minute"),
        3600..86400 => (secs / 3600, "hour"),
        _ => (secs / 86400, "day"),
    };
    format!("{n} {unit}{} ago", if n == 1 { "" } else { "s" })
}

/// What the other computer is called until pairing tells its name.
const SOMEONE: &str = "the other computer";

/// `peer` at the start of a sentence: a name stays exactly as it is, the stand-in is capitalised.
fn opening(peer: &str) -> String {
    if peer == SOMEONE { capitalize(peer) } else { peer.to_string() }
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    const LAPTOP: Screen = Screen { x: 0, y: 0, w: 3840, h: 1080 };
    const PC: Screen = Screen { x: 0, y: 0, w: 2560, h: 1440 };

    #[test]
    fn snaps_to_the_nearest_edge_and_back() {
        // Dropped left of the laptop and a bit low: left edge, the PC's top 1000px touching.
        let l = snap(LAPTOP, Screen { x: -2700, y: 80, ..PC });
        assert_eq!(l.edge, Edge::Left);
        let at = place(LAPTOP, PC, &l);
        assert_eq!((at.x, at.y), (-2560, 80));
        assert_eq!((l.ours.1, l.theirs.0), (65535, 0));
        // A drop close to lining up with the top lines up.
        let l = snap(LAPTOP, Screen { x: 3900, y: 20, ..PC });
        assert_eq!((l.edge, place(LAPTOP, PC, &l).y), (Edge::Right, 0));
        // Far below and only slightly sideways is the bottom edge, and it never floats free.
        let l = snap(LAPTOP, Screen { x: 9000, y: 3000, ..PC });
        assert_eq!(l.edge, Edge::Right);
        let l = snap(LAPTOP, Screen { x: 100, y: 5000, ..PC });
        let at = place(LAPTOP, PC, &l);
        assert_eq!((l.edge, at.y), (Edge::Bottom, 1080));
        assert!(l.ours.1 > l.ours.0 && l.theirs.1 > l.theirs.0);
        // Dropped off the top right corner: a corner, the PC up and to the right.
        let l = snap(LAPTOP, Screen { x: 3900, y: -1500, ..PC });
        assert!(l.corner && l.edge == Edge::Right);
        let at = place(LAPTOP, PC, &l);
        assert_eq!((at.x, at.y), (3840, -1440));
    }

    /// Drives the arrangement canvas with a pointer, as a person would, with no window.
    #[test]
    fn dragging_diagonally_makes_a_corner() {
        let path = std::env::temp_dir().join(format!("yunta-drag-{}", std::process::id())).join("yunta.conf");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, format!("key = {k}\npublic = {k}\nupdates = no\n", k = "ab".repeat(32))).unwrap();
        let mut app = App::new(config::load_from(&path).unwrap(), Page::Arrangement);
        app.status.displays = vec![Screen { x: 0, y: 0, w: 1920, h: 1080 }];
        let ctx = egui::Context::default();
        widgets::install(&ctx);
        let frame = |app: &mut App, events: Vec<egui::Event>| {
            let screen = Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0));
            let mut output =
                ctx.run_ui(egui::RawInput { screen_rect: Some(screen), events, ..Default::default() }, |ui| app.canvas(ui, "PC"));
            output.textures_delta.clear();
        };
        let button =
            |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
        frame(&mut app, vec![]);
        let mut at = app.peer_at.center();
        frame(&mut app, vec![egui::Event::PointerMoved(at)]);
        frame(&mut app, vec![button(at, true)]);
        for _ in 0..30 {
            at += Vec2::new(2.0, -12.0);
            frame(&mut app, vec![egui::Event::PointerMoved(at)]);
        }
        frame(&mut app, vec![button(at, false)]);
        let saved = config::load_from(&path).unwrap().layout;
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
        assert!(saved.corner && saved.edge == Edge::Right && saved.ours == (0, 0), "{saved:?}");
    }

    #[test]
    fn says_how_long_ago() {
        let now = crate::now_ms();
        assert_eq!(ago(now), "just now");
        assert_eq!(ago(now - 60_000), "1 minute ago");
        assert_eq!(ago(now - 3 * 3_600_000), "3 hours ago");
        assert_eq!(ago(now - 2 * 86_400_000), "2 days ago");
    }

    #[test]
    fn status_parses() {
        let s = Status::parse("linked = yes\ninput = there\ndisplays = 0 0 1920 1080; 1920 0 1920 1080\npeer_displays =\n");
        assert!(s.linked);
        assert_eq!((s.input.as_str(), s.displays.len(), s.peer_displays.len()), ("there", 2, 0));
        assert_eq!(fingerprint(&[0xAB; 32]), "abab abab abab abab");
    }
}
