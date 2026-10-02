//! X11 backend. XInput2 raw events watch the pointer and keys everywhere; while driving the
//! peer, a pointer and keyboard grab keeps local apps from seeing the input as well.
//!
//! The grab lives on a second X connection: the server sends no raw events to the client that
//! holds the grab, and raw motion is what still moves when the hidden pointer sits at an edge.
//! XTest plays the peer's input.

use std::cell::Cell;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};

use x11rb::connection::Connection;
use x11rb::protocol::Event as XEvent;
use x11rb::protocol::randr::ConnectionExt as _;
use x11rb::protocol::xinput::{self, ConnectionExt as _};
use x11rb::protocol::xkb::{self, ConnectionExt as _};
use x11rb::protocol::xproto::{self, ConnectionExt as _, GrabMode, GrabStatus};
use x11rb::protocol::xtest::ConnectionExt as _;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;
use x11rb::{CURRENT_TIME, NONE};

use crate::crossing::Rect;
use crate::icon::{self, Look};
use crate::msg::Msg;
use crate::{Event, Input, autostart, keymap};

pub struct Os {
    conn: Arc<RustConnection>,
    grab_conn: Arc<RustConnection>,
    screen: usize,
    root: u32,
    blank: u32,
    grabbed: Arc<AtomicBool>,
    /// Scrolling not yet a whole wheel notch, since X only knows notches.
    scroll: Cell<(i32, i32)>,
    /// _NET_ACTIVE_WINDOW, _NET_WM_STATE and _NET_WM_STATE_FULLSCREEN, for spotting a full-screen app.
    atoms: [u32; 3],
}

impl Os {
    pub fn start(tx: mpsc::Sender<Input>) -> io::Result<Os> {
        let (conn, screen) =
            x11rb::connect(None).map_err(|e| io::Error::other(format!("cannot open the X display: {e} (yunta needs an X11 session)")))?;
        let conn = Arc::new(conn);
        let root = conn.setup().roots[screen].root;
        conn.xinput_xi_query_version(2, 2).map_err(other)?.reply().map_err(|_| io::Error::other("the X server has no XInput 2"))?;
        let mask = xinput::XIEventMask::RAW_MOTION | xinput::XIEventMask::RAW_KEY_PRESS | xinput::XIEventMask::RAW_KEY_RELEASE;
        conn.xinput_xi_select_events(root, &[xinput::EventMask { deviceid: xinput::Device::ALL_MASTER.into(), mask: vec![mask] }])
            .map_err(other)?;
        conn.xtest_get_version(2, 2).map_err(other)?.reply().map_err(|_| io::Error::other("the X server has no XTest"))?;
        conn.flush().map_err(other)?;

        let (grab_conn, _) = x11rb::connect(None).map_err(other)?;
        let grab_conn = Arc::new(grab_conn);
        // A held key then repeats as presses alone, so the peer sees one long press, not a stutter.
        grab_conn.xkb_use_extension(1, 0).map_err(other)?.reply().map_err(other)?;
        let repeat = xkb::PerClientFlag::DETECTABLE_AUTO_REPEAT;
        grab_conn
            .xkb_per_client_flags(xkb::ID::USE_CORE_KBD.into(), repeat, repeat, 0u32.into(), 0u32.into(), 0u32.into())
            .map_err(other)?
            .reply()
            .map_err(other)?;
        let blank = blank_cursor(&grab_conn, root)?;
        // Touchpad swipes, from X servers with XInput 2.4. Their bits run past the first word of
        // the mask: begin and update are bits 30 and 31, end is bit 32.
        let gestures = grab_conn
            .xinput_xi_query_version(2, 4)
            .ok()
            .and_then(|c| c.reply().ok())
            .is_some_and(|v| (v.major_version, v.minor_version) >= (2, 4));
        if gestures {
            let mask = vec![xinput::XIEventMask::from(3u32 << 30), xinput::XIEventMask::from(1u32)];
            let _ = grab_conn.xinput_xi_select_events(root, &[xinput::EventMask { deviceid: xinput::Device::ALL_MASTER.into(), mask }]);
        }
        grab_conn.flush().map_err(other)?;
        let atom = |name: &str| conn.intern_atom(false, name.as_bytes()).ok().and_then(|c| c.reply().ok()).map_or(NONE, |r| r.atom);
        let atoms = [atom("_NET_ACTIVE_WINDOW"), atom("_NET_WM_STATE"), atom("_NET_WM_STATE_FULLSCREEN")];

        let grabbed = Arc::<AtomicBool>::default();
        let os = Os {
            conn: conn.clone(),
            grab_conn: grab_conn.clone(),
            screen,
            root,
            blank,
            grabbed: grabbed.clone(),
            scroll: Cell::default(),
            atoms,
        };
        let (grabbed2, tx2) = (grabbed.clone(), tx.clone());
        let (grabbed3, watched) = (grabbed.clone(), grab_conn.clone());
        std::thread::spawn(move || capture(conn, root, grabbed, tx));
        std::thread::spawn(move || grabbed_input(grab_conn, grabbed2, tx2));
        // The core hung while we hold the keyboard and mouse: let go, so this computer stays usable.
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(std::time::Duration::from_secs(1));
                if grabbed3.load(Ordering::Relaxed) && crate::core_silent() > crate::WATCHDOG {
                    let _ = watched.ungrab_keyboard(CURRENT_TIME);
                    let _ = watched.ungrab_pointer(CURRENT_TIME);
                    let _ = watched.flush();
                    grabbed3.store(false, Ordering::Relaxed);
                    log!("the app stopped answering while driving: giving the keyboard and mouse back");
                }
            }
        });
        Ok(os)
    }

    pub fn displays(&self) -> Vec<Rect> {
        let monitors =
            self.conn.randr_get_monitors(self.root, true).ok().and_then(|c| c.reply().ok()).map(|r| r.monitors).unwrap_or_default();
        let mut out: Vec<Rect> =
            monitors.iter().map(|m| Rect { x: m.x.into(), y: m.y.into(), w: m.width.into(), h: m.height.into() }).collect();
        if out.is_empty() {
            let s = &self.conn.setup().roots[self.screen];
            out.push(Rect { x: 0, y: 0, w: s.width_in_pixels.into(), h: s.height_in_pixels.into() });
        }
        out.retain(Rect::sane);
        out
    }

    pub fn cursor(&self) -> (i32, i32) {
        pointer(&self.conn, self.root).map_or((0, 0), |p| (p.root_x.into(), p.root_y.into()))
    }

    pub fn is_grabbed(&self) -> bool {
        self.grabbed.load(Ordering::Relaxed)
    }

    /// X11 has no mouse hook to count for the log.
    pub fn hook_stats(&self) -> (u32, u32) {
        (0, 0)
    }

    /// A jump to the entry point or back home. Warping is immediate on X11, as moving is.
    pub fn place(&self, x: i32, y: i32) {
        self.move_to(x, y);
    }

    pub fn move_to(&self, x: i32, y: i32) {
        let _ = self.conn.warp_pointer(NONE, self.root, 0, 0, 0, 0, x as i16, y as i16);
        let _ = self.conn.flush();
    }

    /// Takes the pointer and keyboard away from every other app, or gives them back.
    pub fn grab(&self, on: bool) -> bool {
        self.grabbed.store(on, Ordering::Relaxed);
        if !on {
            let _ = self.grab_conn.ungrab_keyboard(CURRENT_TIME);
            let _ = self.grab_conn.ungrab_pointer(CURRENT_TIME);
            let _ = self.grab_conn.flush();
            return true;
        }
        if self.grab_pointer() && self.grab_keyboard() {
            return true;
        }
        self.grab(false);
        false
    }

    fn grab_pointer(&self) -> bool {
        let buttons = xproto::EventMask::BUTTON_PRESS | xproto::EventMask::BUTTON_RELEASE;
        let grab = self.grab_conn.grab_pointer(false, self.root, buttons, GrabMode::ASYNC, GrabMode::ASYNC, NONE, self.blank, CURRENT_TIME);
        grab.ok().and_then(|c| c.reply().ok()).is_some_and(|r| r.status == GrabStatus::SUCCESS)
    }

    fn grab_keyboard(&self) -> bool {
        let grab = self.grab_conn.grab_keyboard(false, self.root, CURRENT_TIME, GrabMode::ASYNC, GrabMode::ASYNC);
        grab.ok().and_then(|c| c.reply().ok()).is_some_and(|r| r.status == GrabStatus::SUCCESS)
    }

    /// The keys and buttons that stay here while driving. X11 grabs all or nothing, so they are
    /// played here by `play_local` instead.
    pub fn keep(&self, _keys: &[u16], _buttons: &[u8]) {}

    /// Plays a key or button meant for this computer while the grab holds everything: lets go,
    /// presses and releases it here, and takes hold again. False if the grab cannot be taken back.
    // ponytail: press and release at once, so a kept key cannot be held down; fine for volume, media and side buttons
    pub fn play_local(&self, msg: &Msg) -> bool {
        let (keyboard, down) = match *msg {
            Msg::Key { down, .. } => (true, down),
            Msg::Button { down, .. } => (false, down),
            _ => return true,
        };
        if !down {
            return true;
        }
        let _ = if keyboard { self.grab_conn.ungrab_keyboard(CURRENT_TIME) } else { self.grab_conn.ungrab_pointer(CURRENT_TIME) };
        let _ = self.grab_conn.flush();
        self.inject(msg);
        let up = match *msg {
            Msg::Key { hid, .. } => Msg::Key { hid, down: false },
            Msg::Button { button, .. } => Msg::Button { button, down: false },
            _ => unreachable!(),
        };
        self.inject(&up);
        let _ = self.conn.sync();
        if keyboard { self.grab_keyboard() } else { self.grab_pointer() }
    }

    /// Where the focused window is, when it is full screen, as a game or a film is.
    pub fn fullscreen(&self) -> Option<Rect> {
        let [active, state, fullscreen] = self.atoms;
        let prop = |window: u32, name: u32| {
            self.conn.get_property(false, window, name, xproto::AtomEnum::ANY, 0, 64).ok().and_then(|c| c.reply().ok())
        };
        let window = prop(self.root, active).and_then(|r| r.value32().and_then(|mut v| v.next())).filter(|w| *w != NONE)?;
        let states: Vec<u32> = prop(window, state).and_then(|r| r.value32().map(|v| v.collect()))?;
        if !states.contains(&fullscreen) {
            return None;
        }
        let size = self.conn.get_geometry(window).ok()?.reply().ok()?;
        let at = self.conn.translate_coordinates(window, self.root, 0, 0).ok()?.reply().ok()?;
        Some(Rect { x: at.dst_x.into(), y: at.dst_y.into(), w: size.width.into(), h: size.height.into() }).filter(Rect::sane)
    }

    pub fn inject(&self, msg: &Msg) {
        match *msg {
            Msg::Key { hid, down } => {
                let Some(code) = keymap::evdev_from_hid(hid) else { return };
                self.fake(if down { xproto::KEY_PRESS_EVENT } else { xproto::KEY_RELEASE_EVENT }, (code + 8) as u8);
            }
            Msg::Button { button, down } => {
                let Some(&detail) = [1, 3, 2, 8, 9].get(usize::from(button).wrapping_sub(1)) else { return };
                self.fake(if down { xproto::BUTTON_PRESS_EVENT } else { xproto::BUTTON_RELEASE_EVENT }, detail);
            }
            Msg::Scroll { dx, dy } => {
                let (mut sx, mut sy) = self.scroll.get();
                // Up is button 4 and down 5; left is 6 and right 7.
                for (clicks, up, down) in [(notches(&mut sy, dy), 4, 5), (notches(&mut sx, dx), 7, 6)] {
                    for _ in 0..clicks.abs() {
                        let b = if clicks > 0 { up } else { down };
                        self.fake(xproto::BUTTON_PRESS_EVENT, b);
                        self.fake(xproto::BUTTON_RELEASE_EVENT, b);
                    }
                }
                self.scroll.set((sx, sy));
            }
            _ => return,
        }
        let _ = self.conn.flush();
    }

    fn fake(&self, kind: u8, detail: u8) {
        let _ = self.conn.xtest_fake_input(kind, detail, CURRENT_TIME, NONE, 0, 0, 0);
    }
}

/// Whole notches in `pending` + `delta` (120 each), keeping the remainder in `pending`.
fn notches(pending: &mut i32, delta: i16) -> i32 {
    *pending += i32::from(delta);
    let whole = *pending / 120;
    *pending -= whole * 120;
    whole
}

// Consoles are a Windows matter: a Linux terminal is already there.
pub fn attach_console() {}

pub fn open_console() -> bool {
    false
}

pub fn close_console() {}

/// The tray icon and its menu, over D-Bus (StatusNotifierItem).
pub struct Tray(ksni::blocking::Handle<TrayModel>);

pub struct TrayModel {
    look: Look,
    status: String,
    autostart: bool,
    tx: mpsc::Sender<Input>,
}

impl Tray {
    pub fn start(tx: mpsc::Sender<Input>) -> Option<Tray> {
        use ksni::blocking::TrayMethods;
        let model = TrayModel { look: Look::Waiting, status: String::new(), autostart: autostart::enabled(), tx };
        match model.spawn() {
            Ok(handle) => Some(Tray(handle)),
            Err(e) => {
                log!("no tray icon: {e}");
                None
            }
        }
    }

    pub fn show(&self, look: Look, status: &str) {
        self.0.update(|m| {
            m.look = look;
            m.status = status.to_string();
        });
    }

    pub fn remove(&self) {
        self.0.shutdown().wait();
    }
}

impl ksni::Tray for TrayModel {
    fn id(&self) -> String {
        "yunta".into()
    }

    fn title(&self) -> String {
        "Yunta".into()
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        // RGBA to the ARGB the spec wants.
        let data = icon::rgba(self.look).chunks(4).flat_map(|p| [p[3], p[0], p[1], p[2]]).collect();
        vec![ksni::Icon { width: icon::SIZE as i32, height: icon::SIZE as i32, data }]
    }

    /// A left click on the icon.
    fn activate(&mut self, _x: i32, _y: i32) {
        crate::open_settings(None);
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip { title: format!("Yunta: {}", self.status), ..Default::default() }
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::{CheckmarkItem, StandardItem};
        vec![
            StandardItem { label: self.status.clone(), enabled: false, ..Default::default() }.into(),
            ksni::MenuItem::Separator,
            StandardItem {
                label: "Settings…".into(), activate: Box::new(|_: &mut Self| crate::open_settings(None)), ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Pair a new computer…".into(),
                activate: Box::new(|_: &mut Self| crate::open_settings(Some("pairing"))),
                ..Default::default()
            }
            .into(),
            CheckmarkItem {
                label: "Pause crossing".into(),
                checked: self.look == Look::Paused,
                activate: Box::new(|m: &mut Self| {
                    let _ = m.tx.send(Input::Pause(m.look != Look::Paused));
                }),
                ..Default::default()
            }
            .into(),
            CheckmarkItem {
                label: "Start at login".into(),
                checked: self.autostart,
                activate: Box::new(|m: &mut Self| match autostart::set(!m.autostart) {
                    Ok(()) => m.autostart = !m.autostart,
                    Err(e) => log!("start at login: {e}"),
                }),
                ..Default::default()
            }
            .into(),
            ksni::MenuItem::Separator,
            StandardItem {
                label: "Quit".into(),
                activate: Box::new(|m: &mut Self| {
                    let _ = m.tx.send(Input::Quit);
                }),
                ..Default::default()
            }
            .into(),
        ]
    }
}

/// Raw motion always, raw keys while not grabbed.
fn capture(conn: Arc<RustConnection>, root: u32, grabbed: Arc<AtomicBool>, tx: mpsc::Sender<Input>) {
    // Motion comes in fractions of a pixel. The remainders carry over so none of it is lost.
    let (mut rx, mut ry) = (0.0, 0.0);
    loop {
        let event = next_event(&conn);
        let grabbed = grabbed.load(Ordering::Relaxed);
        let out = match event {
            XEvent::XinputRawMotion(e) => {
                let (fx, fy) = motion(&e);
                rx += fx;
                ry += fy;
                let (dx, dy) = (rx.trunc(), ry.trunc());
                rx -= dx;
                ry -= dy;
                if dx == 0.0 && dy == 0.0 {
                    continue;
                }
                let (dx, dy) = (dx as i32, dy as i32);
                if grabbed {
                    Some(Event::Motion { x: 0, y: 0, dx, dy, dragging: false, grabbed: true })
                } else {
                    // ponytail: one round trip per motion event, fine at 1000Hz on a local X server
                    pointer(&conn, root).map(|p| Event::Motion {
                        x: p.root_x.into(),
                        y: p.root_y.into(),
                        dx,
                        dy,
                        // Button1Mask to Button3Mask.
                        dragging: u16::from(p.mask) & 0x0700 != 0,
                        grabbed: false,
                    })
                }
            }
            // While grabbed, keys come from the grab instead: raw events carry no auto-repeat.
            XEvent::XinputRawKeyPress(e) if !grabbed => key(e.detail, true),
            XEvent::XinputRawKeyRelease(e) if !grabbed => key(e.detail, false),
            _ => None,
        };
        if let Some(out) = out
            && tx.send(Input::Local(out)).is_err()
        {
            return;
        }
    }
}

/// Keys and buttons, which reach the grab connection only while it holds the grab, and
/// touchpad swipes, which count only then.
fn grabbed_input(conn: Arc<RustConnection>, grabbed: Arc<AtomicBool>, tx: mpsc::Sender<Input>) {
    // Where the current swipe has got to, in pixels.
    let mut swipe = (0.0f64, 0.0f64);
    loop {
        let out = match next_event(&conn) {
            XEvent::XinputGestureSwipeBegin(_) => {
                swipe = (0.0, 0.0);
                None
            }
            XEvent::XinputGestureSwipeUpdate(e) => {
                swipe.0 += f64::from(e.delta_x) / 65536.0;
                swipe.1 += f64::from(e.delta_y) / 65536.0;
                None
            }
            XEvent::XinputGestureSwipeEnd(e) if grabbed.load(Ordering::Relaxed) => {
                let cancelled = u32::from(e.flags) & u32::from(xinput::GestureSwipeEventFlags::GESTURE_SWIPE_CANCELLED) != 0;
                swipe_direction(swipe.0, swipe.1)
                    .filter(|_| !cancelled && (3..=4).contains(&e.detail))
                    .map(|direction| Event::Gesture { fingers: e.detail as u8, direction })
            }
            XEvent::KeyPress(e) => key(e.detail.into(), true),
            XEvent::KeyRelease(e) => key(e.detail.into(), false),
            XEvent::ButtonPress(e) => button(e.detail, true),
            XEvent::ButtonRelease(e) => button(e.detail, false),
            _ => None,
        };
        if let Some(out) = out
            && tx.send(Input::Local(out)).is_err()
        {
            return;
        }
    }
}

/// 0 up, 1 down, 2 left, 3 right, for a swipe that went far enough to mean it.
fn swipe_direction(dx: f64, dy: f64) -> Option<u8> {
    const FAR: f64 = 60.0;
    match (dx.abs() > dy.abs(), dx, dy) {
        (true, dx, _) if dx <= -FAR => Some(2),
        (true, dx, _) if dx >= FAR => Some(3),
        (false, _, dy) if dy <= -FAR => Some(0),
        (false, _, dy) if dy >= FAR => Some(1),
        _ => None,
    }
}

fn next_event(conn: &RustConnection) -> XEvent {
    conn.wait_for_event().unwrap_or_else(|e| {
        log!("X11 connection lost: {e}");
        std::process::exit(1)
    })
}

fn pointer(conn: &RustConnection, root: u32) -> Option<xproto::QueryPointerReply> {
    conn.query_pointer(root).ok()?.reply().ok()
}

/// Accelerated x and y motion from valuators 0 and 1.
fn motion(e: &xinput::RawMotionEvent) -> (f64, f64) {
    let (mut dx, mut dy, mut k) = (0.0, 0.0, 0);
    for bit in 0..(e.valuator_mask.len() * 32).min(2) {
        if e.valuator_mask[bit / 32] & (1 << (bit % 32)) == 0 {
            continue;
        }
        let v = e.axisvalues.get(k).map_or(0.0, |v| v.integral as f64 + v.frac as f64 / 4294967296.0);
        k += 1;
        if bit == 0 { dx = v } else { dy = v }
    }
    (dx, dy)
}

/// X keycodes are evdev codes + 8.
fn key(keycode: u32, down: bool) -> Option<Event> {
    let hid = keymap::hid_from_evdev(u16::try_from(keycode.checked_sub(8)?).ok()?)?;
    Some(Event::Key { hid, down })
}

/// X numbers buttons 1 left, 2 middle, 3 right, 4-7 wheel, 8 back, 9 forward.
fn button(detail: u8, down: bool) -> Option<Event> {
    let button = |button| Some(Event::Button { button, down });
    // A wheel click is a press and a release: count the press only.
    let scroll = |dx, dy| if down { Some(Event::Scroll { dx, dy }) } else { None };
    match detail {
        1 => button(1),
        2 => button(3),
        3 => button(2),
        8 => button(4),
        9 => button(5),
        4 => scroll(0, 120),
        5 => scroll(0, -120),
        6 => scroll(-120, 0),
        7 => scroll(120, 0),
        _ => None,
    }
}

/// An invisible cursor, shown while the pointer is on the peer.
fn blank_cursor(conn: &RustConnection, root: u32) -> io::Result<u32> {
    let pixmap = conn.generate_id().map_err(other)?;
    conn.create_pixmap(1, pixmap, root, 1, 1).map_err(other)?;
    let gc = conn.generate_id().map_err(other)?;
    conn.create_gc(gc, pixmap, &xproto::CreateGCAux::new().foreground(0)).map_err(other)?;
    conn.poly_fill_rectangle(pixmap, gc, &[xproto::Rectangle { x: 0, y: 0, width: 1, height: 1 }]).map_err(other)?;
    let cursor = conn.generate_id().map_err(other)?;
    conn.create_cursor(cursor, pixmap, pixmap, 0, 0, 0, 0, 0, 0, 0, 0).map_err(other)?;
    conn.free_gc(gc).map_err(other)?;
    conn.free_pixmap(pixmap).map_err(other)?;
    Ok(cursor)
}

/// A see-through window for the crossing animations: 32-bit colour with alpha, above everything,
/// out of the window manager's hands, and with no input area, so clicks go through it.
pub struct Overlay {
    conn: RustConnection,
    window: u32,
    gc: u32,
    mapped: bool,
    at: (i32, i32, u32, u32),
}

impl Overlay {
    /// None without a compositor: without one, the alpha would show as black.
    pub fn new() -> Option<Overlay> {
        Overlay::make().map_err(|e| log!("no crossing animations: {e}")).ok().flatten()
    }

    fn make() -> io::Result<Option<Overlay>> {
        use x11rb::protocol::shape::{self, ConnectionExt as _};
        let (conn, screen) = x11rb::connect(None).map_err(other)?;
        let s = &conn.setup().roots[screen];
        let root = s.root;
        let atom = conn.intern_atom(false, format!("_NET_WM_CM_S{screen}").as_bytes()).map_err(other)?.reply().map_err(other)?.atom;
        if conn.get_selection_owner(atom).map_err(other)?.reply().map_err(other)?.owner == NONE {
            return Ok(None);
        }
        let visual = s
            .allowed_depths
            .iter()
            .filter(|d| d.depth == 32)
            .flat_map(|d| &d.visuals)
            .find(|v| v.class == xproto::VisualClass::TRUE_COLOR)
            .map(|v| v.visual_id)
            .ok_or_else(|| io::Error::other("the X server has no 32-bit visual"))?;
        let colormap = conn.generate_id().map_err(other)?;
        conn.create_colormap(xproto::ColormapAlloc::NONE, colormap, root, visual).map_err(other)?;
        let window = conn.generate_id().map_err(other)?;
        let aux = xproto::CreateWindowAux::new().background_pixel(0).border_pixel(0).colormap(colormap).override_redirect(1);
        conn.create_window(32, window, root, 0, 0, 1, 1, 0, xproto::WindowClass::INPUT_OUTPUT, visual, &aux).map_err(other)?;
        conn.shape_rectangles(shape::SO::SET, shape::SK::INPUT, xproto::ClipOrdering::UNSORTED, window, 0, 0, &[]).map_err(other)?;
        let gc = conn.generate_id().map_err(other)?;
        conn.create_gc(gc, window, &xproto::CreateGCAux::new()).map_err(other)?;
        conn.flush().map_err(other)?;
        Ok(Some(Overlay { conn, window, gc, mapped: false, at: (0, 0, 0, 0) }))
    }

    /// Shows `bgra` (premultiplied, `w` x `h`) with its top left corner at (x, y).
    pub fn show(&mut self, x: i32, y: i32, w: u32, h: u32, bgra: &[u8]) {
        if self.at != (x, y, w, h) {
            let aux = xproto::ConfigureWindowAux::new().x(x).y(y).width(w).height(h).stack_mode(xproto::StackMode::ABOVE);
            let _ = self.conn.configure_window(self.window, &aux);
            self.at = (x, y, w, h);
        }
        if !self.mapped {
            let _ = self.conn.map_window(self.window);
            self.mapped = true;
        }
        let _ = self.conn.put_image(xproto::ImageFormat::Z_PIXMAP, self.window, self.gc, w as u16, h as u16, 0, 0, 0, 32, bgra);
        let _ = self.conn.flush();
    }

    pub fn hide(&mut self) {
        if self.mapped {
            let _ = self.conn.unmap_window(self.window);
            let _ = self.conn.flush();
            self.mapped = false;
        }
    }

    /// Windows needs its message queue emptied; X11 needs nothing.
    pub fn pump(&mut self) {}
}

fn other(e: impl std::error::Error + Send + Sync + 'static) -> io::Error {
    io::Error::other(e)
}

#[cfg(test)]
mod tests {
    #[test]
    fn swipes_need_to_go_somewhere() {
        assert_eq!(super::swipe_direction(-200.0, 30.0), Some(2));
        assert_eq!(super::swipe_direction(10.0, 150.0), Some(1));
        assert_eq!(super::swipe_direction(20.0, -15.0), None);
    }

    #[test]
    fn notches_carry_the_remainder() {
        let mut pending = 0;
        assert_eq!(super::notches(&mut pending, 40), 0);
        assert_eq!(super::notches(&mut pending, 90), 1);
        assert_eq!(pending, 10);
        assert_eq!(super::notches(&mut pending, -250), -2);
        assert_eq!(pending, 0);
    }
}
