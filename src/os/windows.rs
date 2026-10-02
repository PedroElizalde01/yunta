//! Windows backend. Low-level hooks see keys, buttons and the wheel, and swallow all of it while
//! this PC drives the peer. Raw Input measures how far the mouse moved, always: a hook reports the
//! cursor clamped to the desktop, and a move it was too slow to stop shifts every later one.
//! SendInput plays the peer's input. The tray icon lives on the same thread and window.

use std::ffi::c_void;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicPtr, AtomicU32, Ordering};
use std::sync::{OnceLock, mpsc};
use std::{io, mem, ptr};

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION, CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS,
    DeleteDC, DeleteObject, EnumDisplayMonitors, GetDC, GetMonitorInfoW, HDC, HMONITOR, MONITOR_DEFAULTTONEAREST, MONITORINFO,
    MonitorFromWindow, ReleaseDC, SelectObject,
};
use windows_sys::Win32::System::Console::{ATTACH_PARENT_PROCESS, AllocConsole, AttachConsole, FreeConsole};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_HIGHEST};
use windows_sys::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE,
    MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP,
    MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK, MOUSEEVENTF_WHEEL, MOUSEEVENTF_XDOWN,
    MOUSEEVENTF_XUP, MOUSEINPUT, SendInput, VK_LBUTTON, VK_MBUTTON, VK_PAUSE, VK_RBUTTON,
};
use windows_sys::Win32::UI::Input::{
    GetRawInputData, HRAWINPUT, MOUSE_MOVE_ABSOLUTE, RAWINPUT, RAWINPUTDEVICE, RAWINPUTHEADER, RID_INPUT, RIDEV_INPUTSINK, RIM_TYPEMOUSE,
    RegisterRawInputDevices,
};
use windows_sys::Win32::UI::Shell::{NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW, Shell_NotifyIconW};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CallNextHookEx, CreateCursor, CreateIcon, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyIcon, DestroyMenu,
    DispatchMessageW, GetClassNameW, GetCursorPos, GetDesktopWindow, GetForegroundWindow, GetMessageW, GetShellWindow, GetSystemMetrics,
    GetWindowRect, HICON, HWND_TOPMOST, KBDLLHOOKSTRUCT, LLKHF_EXTENDED, LLKHF_INJECTED, LLMHF_INJECTED, LWA_ALPHA, MF_CHECKED, MF_GRAYED,
    MF_SEPARATOR, MF_STRING, MSG, MSLLHOOKSTRUCT, PM_REMOVE, PeekMessageW, PostMessageW, RegisterClassW, RegisterWindowMessageW,
    SM_CXCURSOR, SM_CXVIRTUALSCREEN, SM_CYCURSOR, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SPI_SETCURSORS, SW_HIDE,
    SW_SHOWNOACTIVATE, SWP_HIDEWINDOW, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SWP_SHOWWINDOW, SetCursorPos,
    SetForegroundWindow, SetLayeredWindowAttributes, SetWindowPos, SetWindowsHookExW, ShowWindow, SystemParametersInfoW, TPM_NONOTIFY,
    TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu, ULW_ALPHA, UpdateLayeredWindow, WH_KEYBOARD_LL, WH_MOUSE_LL, WM_APP, WM_INPUT,
    WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEHWHEEL, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_RBUTTONDOWN,
    WM_RBUTTONUP, WM_SYSKEYDOWN, WM_XBUTTONDOWN, WM_XBUTTONUP, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    WS_EX_TRANSPARENT, WS_POPUP, XBUTTON1, XBUTTON2,
};
use windows_sys::core::BOOL;

use crate::crossing::Rect;
use crate::icon::{self, Look};
use crate::msg::Msg;
use crate::{Event, Input, autostart, keymap};

/// Stamped on everything we inject. The hooks have a flag for injected input; Raw Input does not.
const MARK: usize = 0x5955_4E54;

// The hook and window procedures take no context, so what they share lives here.
static EVENTS: OnceLock<mpsc::Sender<Input>> = OnceLock::new();
static GRABBED: AtomicBool = AtomicBool::new(false);
/// The hider is up while not grabbed, until this PC's own mouse moves.
static CONCEALED: AtomicBool = AtomicBool::new(false);
/// While grabbed: mouse moves the hook saw, and how many of them had not moved from the park.
static HOOK_MOVES: AtomicU32 = AtomicU32::new(0);
static HOOK_DRIFT: AtomicU32 = AtomicU32::new(0);
/// Keys and mouse buttons that reach Windows even while grabbed: they stay on this PC.
static KEPT: Mutex<(Vec<u16>, Vec<u8>)> = Mutex::new((Vec::new(), Vec::new()));
/// While grabbed or concealed the cursor stays here, where it was, on the hider. Moves are
/// measured by Raw Input, as Beamer does, so the cursor need not sit anywhere in particular.
static PARK: (AtomicI32, AtomicI32) = (AtomicI32::new(0), AtomicI32::new(0));
static WINDOW: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
static TRAY: Mutex<(Look, String)> = Mutex::new((Look::Waiting, String::new()));

const WM_TRAY_CLICK: u32 = WM_APP + 1;
const WM_TRAY_SHOW: u32 = WM_APP + 2;
static HIDER: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
/// The tray icon has been added (rather than needing adding).
static ADDED: AtomicBool = AtomicBool::new(false);
/// Sent to every top-level window when Explorer restarts, which loses everyone's tray icons.
static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);

/// Output goes to the terminal that started us, if any. Nothing appears otherwise.
pub fn attach_console() {
    unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
}

/// A console window of our own, for pairing: the app is built without one.
pub fn open_console() -> bool {
    unsafe { AllocConsole() != 0 }
}

pub fn close_console() {
    unsafe { FreeConsole() };
}

/// The notification-area icon. Its menu is handled on the input thread.
pub struct Tray;

impl Tray {
    pub fn start(_tx: mpsc::Sender<Input>) -> Option<Tray> {
        Some(Tray)
    }

    pub fn show(&self, look: Look, status: &str) {
        *TRAY.lock().unwrap() = (look, status.to_string());
        unsafe { PostMessageW(WINDOW.load(Ordering::Relaxed), WM_TRAY_SHOW, 0, 0) };
    }

    pub fn remove(&self) {
        let data = tray_data(WINDOW.load(Ordering::Relaxed));
        unsafe { Shell_NotifyIconW(NIM_DELETE, &data) };
    }
}

pub struct Os;

impl Os {
    pub fn start(tx: mpsc::Sender<Input>) -> io::Result<Os> {
        // Physical pixels on every monitor, whatever its scaling, so positions line up.
        unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
        // A crash while driving leaves the cursors blank (panic aborts, nothing cleans up).
        restore_cursors();
        EVENTS.set(tx).map_err(|_| io::Error::other("input capture started twice"))?;
        let (ready_tx, ready) = mpsc::channel();
        std::thread::spawn(move || {
            let installed = install();
            let ok = installed.is_ok();
            let _ = ready_tx.send(installed);
            let mut msg: MSG = unsafe { mem::zeroed() };
            while ok && unsafe { GetMessageW(&mut msg, ptr::null_mut(), 0, 0) } > 0 {
                unsafe { DispatchMessageW(&msg) };
            }
        });
        ready.recv().map_err(|_| io::Error::other("the input thread died"))??;
        Ok(Os)
    }

    pub fn displays(&self) -> Vec<Rect> {
        unsafe extern "system" fn each(monitor: HMONITOR, _: HDC, _: *mut RECT, out: LPARAM) -> BOOL {
            let mut info: MONITORINFO = unsafe { mem::zeroed() };
            info.cbSize = mem::size_of::<MONITORINFO>() as u32;
            if unsafe { GetMonitorInfoW(monitor, &mut info) } != 0 {
                let r = info.rcMonitor;
                let out = unsafe { &mut *(out as *mut Vec<Rect>) };
                out.push(Rect { x: r.left, y: r.top, w: r.right - r.left, h: r.bottom - r.top });
            }
            1
        }
        let mut out: Vec<Rect> = Vec::new();
        unsafe { EnumDisplayMonitors(ptr::null_mut(), ptr::null(), Some(each), &mut out as *mut Vec<Rect> as LPARAM) };
        out.retain(Rect::sane);
        out
    }

    pub fn cursor(&self) -> (i32, i32) {
        let mut p = POINT { x: 0, y: 0 };
        unsafe { GetCursorPos(&mut p) };
        (p.x, p.y)
    }

    pub fn move_to(&self, x: i32, y: i32) {
        let metric = |m| unsafe { GetSystemMetrics(m) };
        let (vx, vy) = (metric(SM_XVIRTUALSCREEN), metric(SM_YVIRTUALSCREEN));
        let (vw, vh) = (metric(SM_CXVIRTUALSCREEN).max(1), metric(SM_CYVIRTUALSCREEN).max(1));
        // SendInput spreads 0..65536 over the virtual desktop. Rounding up lands on the exact pixel.
        let norm = |v: i32, origin: i32, len: i32| (((v - origin) as i64 * 65536 + len as i64 - 1) / len as i64) as i32;
        send(mouse(norm(x, vx, vw), norm(y, vy, vh), 0, MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK));
    }

    /// Swallows this PC's input in the hooks, or stops swallowing it.
    pub fn grab(&self, on: bool) -> bool {
        if on {
            // Left where it is, so it never shows anywhere else.
            park_here();
            hide_pointer(true);
        }
        if GRABBED.swap(on, Ordering::Relaxed) != on && !on {
            CONCEALED.store(false, Ordering::Relaxed);
            hide_pointer(false);
        }
        true
    }

    pub fn is_grabbed(&self) -> bool {
        GRABBED.load(Ordering::Relaxed)
    }

    /// Mouse moves the hook swallowed while grabbed, and how many found the cursor off the park
    /// and put it back, since last asked.
    pub fn hook_stats(&self) -> (u32, u32) {
        (HOOK_MOVES.swap(0, Ordering::Relaxed), HOOK_DRIFT.swap(0, Ordering::Relaxed))
    }

    /// Puts the pointer at (x, y) at once. SendInput lands a moment later, so a jump to the
    /// entry point or back home would show the pointer where it was first.
    pub fn place(&self, x: i32, y: i32) {
        unsafe { SetCursorPos(x, y) };
        reveal();
    }

    /// Hides the cursor where it stands, while the other computer is in use. It shows again when
    /// this PC's own mouse moves, or the cursor is placed for a crossing.
    pub fn conceal(&self) {
        if !GRABBED.load(Ordering::Relaxed) && !CONCEALED.swap(true, Ordering::Relaxed) {
            park_here();
            hide_pointer(true);
        }
    }

    /// The keys and buttons that stay here while driving: the hooks let them through.
    pub fn keep(&self, keys: &[u16], buttons: &[u8]) {
        *KEPT.lock().unwrap() = (keys.to_vec(), buttons.to_vec());
    }

    /// Kept keys and buttons already reached Windows through the hooks.
    pub fn play_local(&self, _msg: &Msg) -> bool {
        true
    }

    /// True when the window in front fills its monitor, as a game or a film does. The desktop
    /// itself fills the screen too, and does not count.
    pub fn fullscreen(&self) -> Option<Rect> {
        unsafe {
            let window = GetForegroundWindow();
            if window.is_null() || window == GetShellWindow() || window == GetDesktopWindow() {
                return None;
            }
            let mut class = [0u16; 32];
            let n = GetClassNameW(window, class.as_mut_ptr(), class.len() as i32).max(0) as usize;
            if matches!(String::from_utf16_lossy(&class[..n]).as_str(), "Progman" | "WorkerW") {
                return None;
            }
            let mut rect: RECT = mem::zeroed();
            let mut info: MONITORINFO = mem::zeroed();
            info.cbSize = mem::size_of::<MONITORINFO>() as u32;
            let full = GetWindowRect(window, &mut rect) != 0
                && GetMonitorInfoW(MonitorFromWindow(window, MONITOR_DEFAULTTONEAREST), &mut info) != 0
                && (rect.left, rect.top, rect.right, rect.bottom)
                    == (info.rcMonitor.left, info.rcMonitor.top, info.rcMonitor.right, info.rcMonitor.bottom);
            full.then(|| Rect { x: rect.left, y: rect.top, w: rect.right - rect.left, h: rect.bottom - rect.top }).filter(Rect::sane)
        }
    }

    pub fn inject(&self, msg: &Msg) {
        let input = match *msg {
            Msg::Key { hid, down } => {
                let Some(scan) = keymap::scan_from_hid(hid) else { return };
                let up = if down { 0 } else { KEYEVENTF_KEYUP };
                if let Some(vk) = keymap::media_vk(hid) {
                    key(vk, 0, up)
                } else if hid == 0x48 {
                    // Pause has no scan code SendInput understands, only a virtual key.
                    key(VK_PAUSE, 0, up)
                } else {
                    let extended = if scan > 0xFF { KEYEVENTF_EXTENDEDKEY } else { 0 };
                    key(0, scan & 0xFF, KEYEVENTF_SCANCODE | extended | up)
                }
            }
            Msg::Button { button, down } => {
                let (flags, data) = match (button, down) {
                    (1, true) => (MOUSEEVENTF_LEFTDOWN, 0),
                    (1, false) => (MOUSEEVENTF_LEFTUP, 0),
                    (2, true) => (MOUSEEVENTF_RIGHTDOWN, 0),
                    (2, false) => (MOUSEEVENTF_RIGHTUP, 0),
                    (3, true) => (MOUSEEVENTF_MIDDLEDOWN, 0),
                    (3, false) => (MOUSEEVENTF_MIDDLEUP, 0),
                    (4 | 5, down) => {
                        (if down { MOUSEEVENTF_XDOWN } else { MOUSEEVENTF_XUP }, if button == 4 { XBUTTON1 } else { XBUTTON2 } as u32)
                    }
                    _ => return,
                };
                mouse(0, 0, data, flags)
            }
            Msg::Scroll { dx, dy } => {
                if dx != 0 {
                    send(mouse(0, 0, dx as i32 as u32, MOUSEEVENTF_HWHEEL));
                }
                if dy == 0 {
                    return;
                }
                mouse(0, 0, dy as i32 as u32, MOUSEEVENTF_WHEEL)
            }
            _ => return,
        };
        send(input);
    }
}

/// A see-through window for the crossing animations: layered, above everything, never focused,
/// and clicks go through it.
pub struct Overlay {
    hwnd: HWND,
    shown: bool,
}

impl Overlay {
    pub fn new() -> Option<Overlay> {
        unsafe {
            let module = GetModuleHandleW(ptr::null());
            let class: Vec<u16> = "yunta-fx\0".encode_utf16().collect();
            let wc = WNDCLASSW { lpfnWndProc: Some(DefWindowProcW), hInstance: module, lpszClassName: class.as_ptr(), ..mem::zeroed() };
            // Fails harmlessly when this process registered it already.
            RegisterClassW(&wc);
            let ex = WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE;
            let hwnd = CreateWindowExW(
                ex,
                class.as_ptr(),
                ptr::null(),
                WS_POPUP,
                0,
                0,
                1,
                1,
                ptr::null_mut(),
                ptr::null_mut(),
                module,
                ptr::null(),
            );
            (!hwnd.is_null()).then_some(Overlay { hwnd, shown: false })
        }
    }

    /// Shows `bgra` (premultiplied, `w` x `h`) with its top left corner at (x, y).
    pub fn show(&mut self, x: i32, y: i32, w: u32, h: u32, bgra: &[u8]) {
        unsafe {
            let screen = GetDC(ptr::null_mut());
            let dc = CreateCompatibleDC(screen);
            let mut info: BITMAPINFO = mem::zeroed();
            info.bmiHeader = BITMAPINFOHEADER {
                biSize: mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w as i32,
                // Negative: rows run top down, as in `bgra`.
                biHeight: -(h as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                ..mem::zeroed()
            };
            let mut bits = ptr::null_mut();
            let bitmap = CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut bits, ptr::null_mut(), 0);
            if !bitmap.is_null() && !bits.is_null() {
                ptr::copy_nonoverlapping(bgra.as_ptr(), bits as *mut u8, bgra.len().min((w * h * 4) as usize));
                let old = SelectObject(dc, bitmap);
                let blend =
                    BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, BlendFlags: 0, SourceConstantAlpha: 255, AlphaFormat: AC_SRC_ALPHA as u8 };
                let (at, size, origin) = (POINT { x, y }, SIZE { cx: w as i32, cy: h as i32 }, POINT { x: 0, y: 0 });
                UpdateLayeredWindow(self.hwnd, screen, &at, &size, dc, &origin, 0, &blend, ULW_ALPHA);
                SelectObject(dc, old);
                DeleteObject(bitmap);
            }
            DeleteDC(dc);
            ReleaseDC(ptr::null_mut(), screen);
            if !self.shown {
                ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
                self.shown = true;
            }
        }
    }

    pub fn hide(&mut self) {
        if self.shown {
            unsafe { ShowWindow(self.hwnd, SW_HIDE) };
            self.shown = false;
        }
    }

    /// The window's messages, which only this thread can take.
    pub fn pump(&mut self) {
        let mut msg: MSG = unsafe { mem::zeroed() };
        while unsafe { PeekMessageW(&mut msg, self.hwnd, 0, 0, PM_REMOVE) } != 0 {
            unsafe { DispatchMessageW(&msg) };
        }
    }
}

/// Puts the user's cursor scheme back. Versions before 0.1.3 blanked the system cursors while
/// driving, and a crash then left them blank; this repairs that on start.
fn restore_cursors() {
    unsafe { SystemParametersInfoW(SPI_SETCURSORS, 0, ptr::null_mut(), 0) };
}

/// The hider: a window one pixel square with a blank cursor, put under the parked cursor while
/// this PC drives the peer. The cursor shape comes from the window under it, so it vanishes
/// whatever app is behind, including ones with cursors of their own. Input Leap does the same.
/// Made on the input thread, which owns it.
fn make_hider(module: windows_sys::Win32::Foundation::HINSTANCE) -> HWND {
    unsafe {
        let (w, h) = (GetSystemMetrics(SM_CXCURSOR).max(1), GetSystemMetrics(SM_CYCURSOR).max(1));
        // Monochrome: an AND mask of ones and an XOR mask of zeros is see-through everywhere.
        let bytes = (h * ((w + 15) / 16) * 2) as usize;
        let (and, xor) = (vec![0xFFu8; bytes], vec![0u8; bytes]);
        let blank = CreateCursor(module, 0, 0, w, h, and.as_ptr().cast(), xor.as_ptr().cast());
        let class: Vec<u16> = "yunta-hider\0".encode_utf16().collect();
        let wc = WNDCLASSW {
            lpfnWndProc: Some(DefWindowProcW),
            hInstance: module,
            lpszClassName: class.as_ptr(),
            hCursor: blank,
            ..mem::zeroed()
        };
        RegisterClassW(&wc);
        // Layered at the lowest alpha: nothing to see, yet still under the pointer for hit tests.
        let ex = WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE;
        let hider =
            CreateWindowExW(ex, class.as_ptr(), ptr::null(), WS_POPUP, 0, 0, 1, 1, ptr::null_mut(), ptr::null_mut(), module, ptr::null());
        if !hider.is_null() {
            SetLayeredWindowAttributes(hider, 0, 1, LWA_ALPHA);
        }
        hider
    }
}

/// Shows the hider under the parked cursor, or takes it away. From any thread: Windows hands
/// the move to the input thread, which owns the window, and waits for it. Once the cursor sits
/// on the hider, Windows asks it for a cursor and gets the blank one.
fn park_here() {
    let mut p = POINT { x: 0, y: 0 };
    unsafe { GetCursorPos(&mut p) };
    PARK.0.store(p.x, Ordering::Relaxed);
    PARK.1.store(p.y, Ordering::Relaxed);
}

fn reveal() {
    if CONCEALED.swap(false, Ordering::Relaxed) && !GRABBED.load(Ordering::Relaxed) {
        hide_pointer(false);
    }
}

fn hide_pointer(hide: bool) {
    let hider = HIDER.load(Ordering::Relaxed);
    if hider.is_null() {
        return;
    }
    unsafe {
        if hide {
            let (x, y) = (PARK.0.load(Ordering::Relaxed), PARK.1.load(Ordering::Relaxed));
            SetWindowPos(hider, HWND_TOPMOST, x, y, 1, 1, SWP_NOACTIVATE | SWP_SHOWWINDOW);
        } else {
            SetWindowPos(hider, ptr::null_mut(), 0, 0, 0, 0, SWP_HIDEWINDOW | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
        }
    }
}

fn key(vk: u16, scan: u16, flags: u32) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: vk, wScan: scan, dwFlags: flags, time: 0, dwExtraInfo: MARK } },
    }
}

fn mouse(dx: i32, dy: i32, data: u32, flags: u32) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 { mi: MOUSEINPUT { dx, dy, mouseData: data, dwFlags: flags, time: 0, dwExtraInfo: MARK } },
    }
}

fn send(input: INPUT) {
    unsafe { SendInput(1, &input, mem::size_of::<INPUT>() as i32) };
}

/// Runs on the input thread, which must pump messages for the hooks to be called at all.
fn install() -> io::Result<()> {
    let fail = |what: &str| io::Error::other(format!("{what}: {}", io::Error::last_os_error()));
    unsafe {
        // Windows silently removes a low-level hook that answers too slowly.
        SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_HIGHEST);
        let module = GetModuleHandleW(ptr::null());
        let class: Vec<u16> = "yunta\0".encode_utf16().collect();
        let wc = WNDCLASSW { lpfnWndProc: Some(window_proc), hInstance: module, lpszClassName: class.as_ptr(), ..mem::zeroed() };
        if RegisterClassW(&wc) == 0 {
            return Err(fail("RegisterClassW"));
        }
        // A window never shown, there for Raw Input's WM_INPUT and the tray icon's messages. Not
        // message-only: a tray menu needs a window that can come to the foreground.
        let hwnd = CreateWindowExW(0, class.as_ptr(), ptr::null(), 0, 0, 0, 0, 0, ptr::null_mut(), ptr::null_mut(), module, ptr::null());
        if hwnd.is_null() {
            return Err(fail("CreateWindowExW"));
        }
        WINDOW.store(hwnd, Ordering::Relaxed);
        let name: Vec<u16> = "TaskbarCreated\0".encode_utf16().collect();
        TASKBAR_CREATED.store(RegisterWindowMessageW(name.as_ptr()), Ordering::Relaxed);
        HIDER.store(make_hider(module), Ordering::Relaxed);
        // Generic desktop mouse, delivered even while another window has focus.
        let mouse = RAWINPUTDEVICE { usUsagePage: 1, usUsage: 2, dwFlags: RIDEV_INPUTSINK, hwndTarget: hwnd };
        if RegisterRawInputDevices(&mouse, 1, mem::size_of::<RAWINPUTDEVICE>() as u32) == 0 {
            return Err(fail("RegisterRawInputDevices"));
        }
        if SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), ptr::null_mut(), 0).is_null() {
            return Err(fail("keyboard hook"));
        }
        if SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), ptr::null_mut(), 0).is_null() {
            return Err(fail("mouse hook"));
        }
    }
    Ok(())
}

fn emit(event: Event) {
    if let Some(tx) = EVENTS.get() {
        let _ = tx.send(Input::Local(event));
    }
}

/// Pointer motion, measured by Raw Input whether grabbed or not, and the tray icon.
unsafe extern "system" fn window_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_TRAY_SHOW => show_tray(hwnd),
        // Explorer restarted and dropped the icon: add it again.
        _ if msg != 0 && msg == TASKBAR_CREATED.load(Ordering::Relaxed) => {
            ADDED.store(false, Ordering::Relaxed);
            show_tray(hwnd);
        }
        // A click opens the settings, as tray icons do on Windows; a right click the menu.
        WM_TRAY_CLICK if lparam as u32 == WM_LBUTTONUP => crate::open_settings(None),
        WM_TRAY_CLICK if lparam as u32 == WM_RBUTTONUP => tray_menu(hwnd),
        _ => {}
    }
    if msg == WM_INPUT
        && let Some((dx, dy)) = raw_motion(lparam)
    {
        if GRABBED.load(Ordering::Relaxed) {
            emit(Event::Motion { x: 0, y: 0, dx, dy, dragging: false, grabbed: true });
            return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
        }
        // Our own SendInput is filtered out above, so this is the mouse here.
        reveal();
        let mut p = POINT { x: 0, y: 0 };
        let held = |vk: u16| unsafe { GetAsyncKeyState(vk as i32) } < 0;
        unsafe { GetCursorPos(&mut p) };
        emit(Event::Motion { x: p.x, y: p.y, dx, dy, dragging: held(VK_LBUTTON) || held(VK_RBUTTON) || held(VK_MBUTTON), grabbed: false });
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// The hand's own mouse counts in one WM_INPUT.
fn raw_motion(lparam: LPARAM) -> Option<(i32, i32)> {
    let mut raw: RAWINPUT = unsafe { mem::zeroed() };
    let mut size = mem::size_of::<RAWINPUT>() as u32;
    let header = mem::size_of::<RAWINPUTHEADER>() as u32;
    let read = unsafe { GetRawInputData(lparam as HRAWINPUT, RID_INPUT, &mut raw as *mut RAWINPUT as *mut c_void, &mut size, header) };
    if read == u32::MAX || raw.header.dwType != RIM_TYPEMOUSE {
        return None;
    }
    let m = unsafe { raw.data.mouse };
    // A tablet or remote desktop reports positions, with no push to measure; MARK is our own.
    if m.usFlags & MOUSE_MOVE_ABSOLUTE != 0 || m.ulExtraInformation as usize == MARK || (m.lLastX == 0 && m.lLastY == 0) {
        return None;
    }
    Some((m.lLastX, m.lLastY))
}

/// Every key goes to the core, for the double-tap. While grabbed, none reaches Windows.
/// The core hung while we hold the keyboard and mouse: let go, so this PC stays usable.
fn watchdog() {
    if GRABBED.load(Ordering::Relaxed) && crate::core_silent() > crate::WATCHDOG {
        GRABBED.store(false, Ordering::Relaxed);
        hide_pointer(false);
        log!("the app stopped answering while driving: giving this PC's keyboard and mouse back");
    }
}

unsafe extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    watchdog();
    if code >= 0 {
        let k = unsafe { &*(lparam as *const KBDLLHOOKSTRUCT) };
        if k.flags & LLKHF_INJECTED == 0 {
            let down = matches!(wparam as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
            // Not masked to a byte: AltGr's fake Left Ctrl comes as 0x21D and must not match 0x1D.
            let scan = if k.flags & LLKHF_EXTENDED != 0 { 0xE000 | k.scanCode } else { k.scanCode };
            if let Some(hid) = u16::try_from(scan).ok().and_then(keymap::hid_from_scan) {
                emit(Event::Key { hid, down });
            }
            let kept = u16::try_from(scan).ok().and_then(keymap::hid_from_scan).is_some_and(|hid| KEPT.lock().unwrap().0.contains(&hid));
            if GRABBED.load(Ordering::Relaxed) && !kept {
                return 1;
            }
        }
    }
    unsafe { CallNextHookEx(ptr::null_mut(), code, wparam, lparam) }
}

/// Only while grabbed: everything the mouse does goes to the core instead of Windows.
unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    watchdog();
    if code >= 0 && GRABBED.load(Ordering::Relaxed) {
        let m = unsafe { &*(lparam as *const MSLLHOOKSTRUCT) };
        if m.flags & LLMHF_INJECTED == 0 {
            let wheel = (m.mouseData >> 16) as u16 as i16;
            let msg = wparam as u32;
            let down = matches!(msg, WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN | WM_XBUTTONDOWN);
            let event = match msg {
                // Raw Input measures the move (window_proc); here it is only swallowed. A move
                // the hook was too slow to stop still moved the cursor, so it goes back on the
                // hider, as Beamer puts it back on its pin.
                WM_MOUSEMOVE => {
                    HOOK_MOVES.fetch_add(1, Ordering::Relaxed);
                    let (mut p, park) = (POINT { x: 0, y: 0 }, (PARK.0.load(Ordering::Relaxed), PARK.1.load(Ordering::Relaxed)));
                    if unsafe { GetCursorPos(&mut p) } != 0 && (p.x, p.y) != park {
                        HOOK_DRIFT.fetch_add(1, Ordering::Relaxed);
                        unsafe { SetCursorPos(park.0, park.1) };
                    }
                    None
                }
                WM_LBUTTONDOWN | WM_LBUTTONUP => Some(Event::Button { button: 1, down }),
                WM_RBUTTONDOWN | WM_RBUTTONUP => Some(Event::Button { button: 2, down }),
                WM_MBUTTONDOWN | WM_MBUTTONUP => Some(Event::Button { button: 3, down }),
                WM_XBUTTONDOWN | WM_XBUTTONUP => Some(Event::Button { button: if wheel as u16 == XBUTTON2 { 5 } else { 4 }, down }),
                WM_MOUSEWHEEL => Some(Event::Scroll { dx: 0, dy: wheel }),
                WM_MOUSEHWHEEL => Some(Event::Scroll { dx: wheel, dy: 0 }),
                _ => None,
            };
            let kept = matches!(event, Some(Event::Button { button, .. }) if KEPT.lock().unwrap().1.contains(&button));
            if let Some(event) = event {
                emit(event);
            }
            if !kept {
                return 1;
            }
        }
    }
    unsafe { CallNextHookEx(ptr::null_mut(), code, wparam, lparam) }
}

fn tray_data(hwnd: HWND) -> NOTIFYICONDATAW {
    let mut data: NOTIFYICONDATAW = unsafe { mem::zeroed() };
    data.cbSize = mem::size_of::<NOTIFYICONDATAW>() as u32;
    data.hWnd = hwnd;
    data.uID = 1;
    data
}

/// Adds the icon the first time, updates it after.
fn show_tray(hwnd: HWND) {
    static ICON: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
    let (look, status) = TRAY.lock().unwrap().clone();
    let mut data = tray_data(hwnd);
    data.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
    data.uCallbackMessage = WM_TRAY_CLICK;
    data.hIcon = make_icon(look);
    let tip: Vec<u16> = format!("Yunta: {status}").encode_utf16().take(data.szTip.len() - 1).collect();
    data.szTip[..tip.len()].copy_from_slice(&tip);
    let verb = if ADDED.swap(true, Ordering::Relaxed) { NIM_MODIFY } else { NIM_ADD };
    unsafe { Shell_NotifyIconW(verb, &data) };
    let old = ICON.swap(data.hIcon, Ordering::Relaxed);
    if !old.is_null() {
        unsafe { DestroyIcon(old) };
    }
}

fn make_icon(look: Look) -> HICON {
    // Windows wants BGRA, and an AND mask that the alpha channel makes redundant.
    let bgra: Vec<u8> = icon::rgba(look).chunks(4).flat_map(|p| [p[2], p[1], p[0], p[3]]).collect();
    let mask = [0u8; icon::SIZE * icon::SIZE / 8];
    let size = icon::SIZE as i32;
    unsafe { CreateIcon(ptr::null_mut(), size, size, 1, 32, mask.as_ptr(), bgra.as_ptr()) }
}

fn tray_menu(hwnd: HWND) {
    const PAUSE: usize = 1;
    const AUTOSTART: usize = 2;
    const QUIT: usize = 3;
    const SETTINGS: usize = 4;
    const PAIR: usize = 5;
    let (look, status) = TRAY.lock().unwrap().clone();
    let paused = look == Look::Paused;
    let autostart = autostart::enabled();
    let wide = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
    let (status, settings, pair) = (wide(&status), wide("Settings…"), wide("Pair a new computer…"));
    let (pause, login, quit) = (wide("Pause crossing"), wide("Start at login"), wide("Quit"));
    let checked = |on: bool| if on { MF_CHECKED } else { 0 };
    let chosen = unsafe {
        let menu = CreatePopupMenu();
        AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, status.as_ptr());
        AppendMenuW(menu, MF_SEPARATOR, 0, ptr::null());
        AppendMenuW(menu, MF_STRING, SETTINGS, settings.as_ptr());
        AppendMenuW(menu, MF_STRING, PAIR, pair.as_ptr());
        AppendMenuW(menu, MF_STRING | checked(paused), PAUSE, pause.as_ptr());
        AppendMenuW(menu, MF_STRING | checked(autostart), AUTOSTART, login.as_ptr());
        AppendMenuW(menu, MF_SEPARATOR, 0, ptr::null());
        AppendMenuW(menu, MF_STRING, QUIT, quit.as_ptr());
        let mut at = POINT { x: 0, y: 0 };
        GetCursorPos(&mut at);
        // Without this the menu does not close when clicking elsewhere.
        SetForegroundWindow(hwnd);
        let chosen = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON, at.x, at.y, 0, hwnd, ptr::null());
        DestroyMenu(menu);
        chosen as usize
    };
    let send = |input| {
        if let Some(tx) = EVENTS.get() {
            let _ = tx.send(input);
        }
    };
    match chosen {
        PAUSE => send(Input::Pause(!paused)),
        AUTOSTART => {
            if let Err(e) = autostart::set(!autostart) {
                log!("start at login: {e}");
            }
        }
        QUIT => send(Input::Quit),
        SETTINGS => crate::open_settings(None),
        PAIR => crate::open_settings(Some("pairing")),
        _ => {}
    }
}
