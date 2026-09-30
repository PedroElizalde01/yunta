//! Windows backend. Takes input from the peer with SendInput. Capturing this PC's own input
//! with low-level hooks, so it can drive the peer too, lands with A3.

use std::{io, mem, ptr, sync::mpsc};

use windows_sys::Win32::Foundation::{LPARAM, POINT, RECT};
use windows_sys::Win32::Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO};
use windows_sys::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE,
    MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP,
    MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK, MOUSEEVENTF_WHEEL, MOUSEEVENTF_XDOWN,
    MOUSEEVENTF_XUP, MOUSEINPUT, SendInput, VK_PAUSE,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetCursorPos, GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, XBUTTON1, XBUTTON2,
};
use windows_sys::core::BOOL;

use crate::crossing::Rect;
use crate::msg::Msg;
use crate::{Input, keymap};

pub struct Os;

impl Os {
    pub fn start(_tx: mpsc::Sender<Input>) -> io::Result<Os> {
        // Physical pixels on every monitor, whatever its scaling, so positions line up.
        unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
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

    // ponytail: low-level hooks land with A3, until then this PC is only driven
    pub fn grab(&self, _on: bool) -> bool {
        false
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
        Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: vk, wScan: scan, dwFlags: flags, time: 0, dwExtraInfo: 0 } },
    }
}

fn mouse(dx: i32, dy: i32, data: u32, flags: u32) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 { mi: MOUSEINPUT { dx, dy, mouseData: data, dwFlags: flags, time: 0, dwExtraInfo: 0 } },
    }
}

fn send(input: INPUT) {
    unsafe { SendInput(1, &input, mem::size_of::<INPUT>() as i32) };
}
