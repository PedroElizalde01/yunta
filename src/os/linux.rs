//! X11 backend. XInput2 raw events watch the pointer and keys everywhere; while driving the
//! peer, a pointer and keyboard grab keeps local apps from seeing the input as well.
//!
//! The grab lives on a second X connection: the server sends no raw events to the client that
//! holds the grab, and raw motion is what still moves when the hidden pointer sits at an edge.

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};

use x11rb::connection::Connection;
use x11rb::protocol::Event as XEvent;
use x11rb::protocol::randr::ConnectionExt as _;
use x11rb::protocol::xinput::{self, ConnectionExt as _};
use x11rb::protocol::xkb::{self, ConnectionExt as _};
use x11rb::protocol::xproto::{self, ConnectionExt as _, GrabMode, GrabStatus};
use x11rb::rust_connection::RustConnection;
use x11rb::{CURRENT_TIME, NONE};

use crate::crossing::Rect;
use crate::msg::Msg;
use crate::{Event, Input, keymap};

pub struct Os {
    conn: Arc<RustConnection>,
    grab_conn: Arc<RustConnection>,
    screen: usize,
    root: u32,
    blank: u32,
    grabbed: Arc<AtomicBool>,
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
        grab_conn.flush().map_err(other)?;

        let os = Os { conn: conn.clone(), grab_conn: grab_conn.clone(), screen, root, blank, grabbed: Arc::default() };
        let grabbed = os.grabbed.clone();
        let tx2 = tx.clone();
        std::thread::spawn(move || capture(conn, root, grabbed, tx));
        std::thread::spawn(move || grabbed_input(grab_conn, tx2));
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
        out
    }

    pub fn cursor(&self) -> (i32, i32) {
        pointer(&self.conn, self.root).map_or((0, 0), |p| (p.root_x.into(), p.root_y.into()))
    }

    pub fn move_to(&self, x: i32, y: i32) {
        let _ = self.conn.warp_pointer(NONE, self.root, 0, 0, 0, 0, x as i16, y as i16);
        let _ = self.conn.flush();
    }

    /// Takes the pointer and keyboard away from every other app, or gives them back.
    pub fn grab(&self, on: bool) -> bool {
        self.grabbed.store(on, Ordering::Relaxed);
        let conn = &self.grab_conn;
        if !on {
            let _ = conn.ungrab_keyboard(CURRENT_TIME);
            let _ = conn.ungrab_pointer(CURRENT_TIME);
            let _ = conn.flush();
            return true;
        }
        let buttons = xproto::EventMask::BUTTON_PRESS | xproto::EventMask::BUTTON_RELEASE;
        let pointer = conn.grab_pointer(false, self.root, buttons, GrabMode::ASYNC, GrabMode::ASYNC, NONE, self.blank, CURRENT_TIME);
        let keyboard = conn.grab_keyboard(false, self.root, CURRENT_TIME, GrabMode::ASYNC, GrabMode::ASYNC);
        let ok = |status: Option<GrabStatus>| status == Some(GrabStatus::SUCCESS);
        if ok(pointer.ok().and_then(|c| c.reply().ok()).map(|r| r.status))
            && ok(keyboard.ok().and_then(|c| c.reply().ok()).map(|r| r.status))
        {
            return true;
        }
        self.grab(false);
        false
    }

    // ponytail: injection (XTest) lands with A3, until then this machine only drives
    pub fn inject(&self, _msg: &Msg) {}
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
                    Some(Event::Motion { x: 0, y: 0, dx, dy, dragging: false })
                } else {
                    // ponytail: one round trip per motion event, fine at 1000Hz on a local X server
                    pointer(&conn, root).map(|p| Event::Motion {
                        x: p.root_x.into(),
                        y: p.root_y.into(),
                        dx,
                        dy,
                        // Button1Mask to Button3Mask.
                        dragging: u16::from(p.mask) & 0x0700 != 0,
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

/// Keys and buttons, which reach the grab connection only while it holds the grab.
fn grabbed_input(conn: Arc<RustConnection>, tx: mpsc::Sender<Input>) {
    loop {
        let out = match next_event(&conn) {
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

fn next_event(conn: &RustConnection) -> XEvent {
    conn.wait_for_event().unwrap_or_else(|e| {
        eprintln!("X11 connection lost: {e}");
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

fn other(e: impl std::error::Error + Send + Sync + 'static) -> io::Error {
    io::Error::other(e)
}
