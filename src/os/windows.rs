//! Windows backend. Low-level hooks see keys, buttons and the wheel, and swallow all of it while
//! this PC drives the peer. Raw Input measures how far the mouse moved: a hook reports the cursor
//! already clamped to the desktop, so a push against the edge would never register there.
//! SendInput plays the peer's input.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{OnceLock, mpsc};
use std::{io, mem, ptr};

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO};
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
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, CreateWindowExW, DefWindowProcW, DispatchMessageW, GetCursorPos, GetMessageW, GetSystemMetrics, HWND_MESSAGE,
    KBDLLHOOKSTRUCT, LLKHF_EXTENDED, LLKHF_INJECTED, LLMHF_INJECTED, MSG, MSLLHOOKSTRUCT, RegisterClassW, SM_CXSCREEN, SM_CXVIRTUALSCREEN,
    SM_CYSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SetCursorPos, SetWindowsHookExW, WH_KEYBOARD_LL, WH_MOUSE_LL,
    WM_INPUT, WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEHWHEEL, WM_MOUSEMOVE, WM_MOUSEWHEEL,
    WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SYSKEYDOWN, WM_XBUTTONDOWN, WM_XBUTTONUP, WNDCLASSW, XBUTTON1, XBUTTON2,
};
use windows_sys::core::BOOL;

use crate::crossing::Rect;
use crate::msg::Msg;
use crate::{Event, Input, keymap};

/// Stamped on everything we inject. The hooks have a flag for injected input; Raw Input does not.
const MARK: usize = 0x5955_4E54;

// The hook and window procedures take no context, so what they share lives here.
static EVENTS: OnceLock<mpsc::Sender<Input>> = OnceLock::new();
static GRABBED: AtomicBool = AtomicBool::new(false);
/// While grabbed the cursor is parked here, and each swallowed move is measured from it. That
/// keeps Windows' pointer acceleration, which Raw Input's counts do not have.
static PARK: (AtomicI32, AtomicI32) = (AtomicI32::new(0), AtomicI32::new(0));

pub struct Os;

impl Os {
    pub fn start(tx: mpsc::Sender<Input>) -> io::Result<Os> {
        // Physical pixels on every monitor, whatever its scaling, so positions line up.
        unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
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
            // ponytail: the parked cursor stays visible in the middle of the primary display
            let (x, y) = unsafe { (GetSystemMetrics(SM_CXSCREEN) / 2, GetSystemMetrics(SM_CYSCREEN) / 2) };
            PARK.0.store(x, Ordering::Relaxed);
            PARK.1.store(y, Ordering::Relaxed);
            unsafe { SetCursorPos(x, y) };
        }
        GRABBED.store(on, Ordering::Relaxed);
        true
    }

    pub fn inject(&self, msg: &Msg) {
        let input = match *msg {
            Msg::Key { hid, down } => {
                let Some(scan) = keymap::scan_from_hid(hid) else { return };
                let up = if down { 0 } else { KEYEVENTF_KEYUP };
                if hid == 0x48 {
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
        // A message-only window, there for Raw Input to deliver WM_INPUT to.
        let hwnd = CreateWindowExW(0, class.as_ptr(), ptr::null(), 0, 0, 0, 0, 0, HWND_MESSAGE, ptr::null_mut(), module, ptr::null());
        if hwnd.is_null() {
            return Err(fail("CreateWindowExW"));
        }
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

/// Pointer motion while not grabbed, for pushing against the edge.
unsafe extern "system" fn window_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_INPUT
        && !GRABBED.load(Ordering::Relaxed)
        && let Some((dx, dy)) = raw_motion(lparam)
    {
        let mut p = POINT { x: 0, y: 0 };
        let held = |vk: u16| unsafe { GetAsyncKeyState(vk as i32) } < 0;
        unsafe { GetCursorPos(&mut p) };
        emit(Event::Motion { x: p.x, y: p.y, dx, dy, dragging: held(VK_LBUTTON) || held(VK_RBUTTON) || held(VK_MBUTTON) });
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
unsafe extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        let k = unsafe { &*(lparam as *const KBDLLHOOKSTRUCT) };
        if k.flags & LLKHF_INJECTED == 0 {
            let down = matches!(wparam as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
            // Not masked to a byte: AltGr's fake Left Ctrl comes as 0x21D and must not match 0x1D.
            let scan = if k.flags & LLKHF_EXTENDED != 0 { 0xE000 | k.scanCode } else { k.scanCode };
            if let Some(hid) = u16::try_from(scan).ok().and_then(keymap::hid_from_scan) {
                emit(Event::Key { hid, down });
            }
            if GRABBED.load(Ordering::Relaxed) {
                return 1;
            }
        }
    }
    unsafe { CallNextHookEx(ptr::null_mut(), code, wparam, lparam) }
}

/// Only while grabbed: everything the mouse does goes to the core instead of Windows.
unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 && GRABBED.load(Ordering::Relaxed) {
        let m = unsafe { &*(lparam as *const MSLLHOOKSTRUCT) };
        if m.flags & LLMHF_INJECTED == 0 {
            let wheel = (m.mouseData >> 16) as u16 as i16;
            let msg = wparam as u32;
            let down = matches!(msg, WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN | WM_XBUTTONDOWN);
            let event = match msg {
                WM_MOUSEMOVE => {
                    let (dx, dy) = (m.pt.x - PARK.0.load(Ordering::Relaxed), m.pt.y - PARK.1.load(Ordering::Relaxed));
                    (dx != 0 || dy != 0).then_some(Event::Motion { x: 0, y: 0, dx, dy, dragging: false })
                }
                WM_LBUTTONDOWN | WM_LBUTTONUP => Some(Event::Button { button: 1, down }),
                WM_RBUTTONDOWN | WM_RBUTTONUP => Some(Event::Button { button: 2, down }),
                WM_MBUTTONDOWN | WM_MBUTTONUP => Some(Event::Button { button: 3, down }),
                WM_XBUTTONDOWN | WM_XBUTTONUP => Some(Event::Button { button: if wheel as u16 == XBUTTON2 { 5 } else { 4 }, down }),
                WM_MOUSEWHEEL => Some(Event::Scroll { dx: 0, dy: wheel }),
                WM_MOUSEHWHEEL => Some(Event::Scroll { dx: wheel, dy: 0 }),
                _ => None,
            };
            if let Some(event) = event {
                emit(event);
            }
            return 1;
        }
    }
    unsafe { CallNextHookEx(ptr::null_mut(), code, wparam, lparam) }
}
