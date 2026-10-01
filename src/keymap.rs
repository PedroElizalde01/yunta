//! USB HID keyboard usages (the wire format) to and from each platform's key codes.
//! Positional by nature: a key arrives as the key in the same place on the other machine.

/// (HID usage, Linux evdev code, Windows set-1 scan code with 0xE0 prefix as 0xE0xx).
/// X11 keycodes are evdev + 8. Power is left out on purpose: it must never cross.
#[rustfmt::skip]
const KEYS: &[(u16, u16, u16)] = &[
    (0x04, 30, 0x1E), (0x05, 48, 0x30), (0x06, 46, 0x2E), (0x07, 32, 0x20), // A B C D
    (0x08, 18, 0x12), (0x09, 33, 0x21), (0x0A, 34, 0x22), (0x0B, 35, 0x23), // E F G H
    (0x0C, 23, 0x17), (0x0D, 36, 0x24), (0x0E, 37, 0x25), (0x0F, 38, 0x26), // I J K L
    (0x10, 50, 0x32), (0x11, 49, 0x31), (0x12, 24, 0x18), (0x13, 25, 0x19), // M N O P
    (0x14, 16, 0x10), (0x15, 19, 0x13), (0x16, 31, 0x1F), (0x17, 20, 0x14), // Q R S T
    (0x18, 22, 0x16), (0x19, 47, 0x2F), (0x1A, 17, 0x11), (0x1B, 45, 0x2D), // U V W X
    (0x1C, 21, 0x15), (0x1D, 44, 0x2C),                                     // Y Z
    (0x1E, 2, 0x02), (0x1F, 3, 0x03), (0x20, 4, 0x04), (0x21, 5, 0x05), (0x22, 6, 0x06), // 1-5
    (0x23, 7, 0x07), (0x24, 8, 0x08), (0x25, 9, 0x09), (0x26, 10, 0x0A), (0x27, 11, 0x0B), // 6-0
    (0x28, 28, 0x1C), (0x29, 1, 0x01), (0x2A, 14, 0x0E), (0x2B, 15, 0x0F), // Enter Esc Backspace Tab
    (0x2C, 57, 0x39), (0x2D, 12, 0x0C), (0x2E, 13, 0x0D), (0x2F, 26, 0x1A), // Space - = [
    (0x30, 27, 0x1B), (0x31, 43, 0x2B), (0x32, 43, 0x2B), (0x33, 39, 0x27), // ] \ non-US # ;
    (0x34, 40, 0x28), (0x35, 41, 0x29), (0x36, 51, 0x33), (0x37, 52, 0x34), // ' ` , .
    (0x38, 53, 0x35), (0x39, 58, 0x3A),                                     // / CapsLock
    (0x3A, 59, 0x3B), (0x3B, 60, 0x3C), (0x3C, 61, 0x3D), (0x3D, 62, 0x3E), // F1-F4
    (0x3E, 63, 0x3F), (0x3F, 64, 0x40), (0x40, 65, 0x41), (0x41, 66, 0x42), // F5-F8
    (0x42, 67, 0x43), (0x43, 68, 0x44), (0x44, 87, 0x57), (0x45, 88, 0x58), // F9-F12
    (0x46, 99, 0xE037), (0x47, 70, 0x46),                                   // PrintScreen ScrollLock
    // Pause is E1 1D 45 on the wire and 0x45 in a Windows hook: the Windows backend special-cases it.
    (0x48, 119, 0x45),
    (0x49, 110, 0xE052), (0x4A, 102, 0xE047), (0x4B, 104, 0xE049), // Insert Home PageUp
    (0x4C, 111, 0xE053), (0x4D, 107, 0xE04F), (0x4E, 109, 0xE051), // Delete End PageDown
    (0x4F, 106, 0xE04D), (0x50, 105, 0xE04B), (0x51, 108, 0xE050), (0x52, 103, 0xE048), // → ← ↓ ↑
    (0x53, 69, 0xE045), (0x54, 98, 0xE035), (0x55, 55, 0x37), (0x56, 74, 0x4A), // NumLock KP/ KP* KP-
    (0x57, 78, 0x4E), (0x58, 96, 0xE01C),                                       // KP+ KPEnter
    (0x59, 79, 0x4F), (0x5A, 80, 0x50), (0x5B, 81, 0x51), (0x5C, 75, 0x4B), (0x5D, 76, 0x4C), // KP1-5
    (0x5E, 77, 0x4D), (0x5F, 71, 0x47), (0x60, 72, 0x48), (0x61, 73, 0x49), (0x62, 82, 0x52), // KP6-0
    (0x63, 83, 0x53), (0x64, 86, 0x56), (0x65, 127, 0xE05D), (0x67, 117, 0x59), // KP. non-US \ Menu KP=
    (0x68, 183, 0x64), (0x69, 184, 0x65), (0x6A, 185, 0x66), (0x6B, 186, 0x67), // F13-F16
    (0x6C, 187, 0x68), (0x6D, 188, 0x69), (0x6E, 189, 0x6A), (0x6F, 190, 0x6B), // F17-F20
    (0x70, 191, 0x6C), (0x71, 192, 0x6D), (0x72, 193, 0x6E), (0x73, 194, 0x76), // F21-F24
    (0x7F, 113, 0xE020), (0x80, 115, 0xE030), (0x81, 114, 0xE02E), // Mute VolumeUp VolumeDown
    (0x87, 89, 0x73), (0x88, 93, 0x70), (0x89, 124, 0x7D), (0x8A, 92, 0x79), (0x8B, 94, 0x7B), // JIS
    (0xE0, 29, 0x1D), (0xE1, 42, 0x2A), (0xE2, 56, 0x38), (0xE3, 125, 0xE05B), // LCtrl LShift LAlt LSuper
    (0xE4, 97, 0xE01D), (0xE5, 54, 0x36), (0xE6, 100, 0xE038), (0xE7, 126, 0xE05C), // right-hand ones
    // Media keys live on HID's consumer page, not the keyboard one. They travel as these
    // keyboard-page usages, which HID leaves reserved.
    (PLAY_PAUSE, 164, 0xE022), (NEXT_TRACK, 163, 0xE019), (PREVIOUS_TRACK, 165, 0xE010), (STOP, 166, 0xE024),
];

pub const PLAY_PAUSE: u16 = 0xF0;
pub const NEXT_TRACK: u16 = 0xF1;
pub const PREVIOUS_TRACK: u16 = 0xF2;
pub const STOP: u16 = 0xF3;
pub const MUTE: u16 = 0x7F;
pub const VOLUME_UP: u16 = 0x80;
pub const VOLUME_DOWN: u16 = 0x81;
pub const PRINT_SCREEN: u16 = 0x46;
pub const LEFT_CTRL: u16 = 0xE0;
pub const LEFT_SUPER: u16 = 0xE3;
pub const RIGHT_CTRL: u16 = 0xE4;
pub const RIGHT_SUPER: u16 = 0xE7;

/// The Windows virtual key for keys Windows only acts on when sent as one: the media and volume
/// keys. Sent as scan codes they arrive but do nothing.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn media_vk(hid: u16) -> Option<u16> {
    Some(match hid {
        PLAY_PAUSE => 0xB3,
        NEXT_TRACK => 0xB0,
        PREVIOUS_TRACK => 0xB1,
        STOP => 0xB2,
        MUTE => 0xAD,
        VOLUME_DOWN => 0xAE,
        VOLUME_UP => 0xAF,
        _ => return None,
    })
}

/// Ctrl and Super trade places, so a Mac keyboard's Cmd, which Linux reads as Super, works as
/// Ctrl on the other computer.
pub fn swap_modifiers(hid: u16) -> u16 {
    match hid {
        LEFT_CTRL => LEFT_SUPER,
        LEFT_SUPER => LEFT_CTRL,
        RIGHT_CTRL => RIGHT_SUPER,
        RIGHT_SUPER => RIGHT_CTRL,
        other => other,
    }
}

/// The Windows shortcut a touchpad swipe plays, as the keys to press in order. Windows' own
/// touchpad does the same: up for Task View, down for the desktop, sideways with three fingers
/// to switch apps and with four to switch desktops.
pub fn gesture_keys(fingers: u8, direction: u8) -> &'static [u16] {
    const ALT: u16 = 0xE2;
    const SHIFT: u16 = 0xE1;
    const TAB: u16 = 0x2B;
    match (fingers, direction) {
        (_, 0) => &[LEFT_SUPER, TAB],
        (_, 1) => &[LEFT_SUPER, 0x07], // D
        (3, 2) => &[ALT, TAB],
        (3, 3) => &[ALT, SHIFT, TAB],
        // Fingers left bring in the desktop on the right, as on a real touchpad.
        (_, 2) => &[LEFT_CTRL, LEFT_SUPER, 0x4F],
        (_, 3) => &[LEFT_CTRL, LEFT_SUPER, 0x50],
        _ => &[],
    }
}

/// The keys in a group `keep` can name; `side_buttons` is mouse buttons, see `kept_buttons`.
pub fn kept_keys(group: &str) -> &'static [u16] {
    match group {
        "volume" => &[MUTE, VOLUME_UP, VOLUME_DOWN],
        "media" => &[PLAY_PAUSE, NEXT_TRACK, PREVIOUS_TRACK, STOP],
        "print_screen" => &[PRINT_SCREEN],
        _ => &[],
    }
}

/// Back and forward, the mouse's side buttons.
pub fn kept_buttons(group: &str) -> &'static [u8] {
    if group == "side_buttons" { &[4, 5] } else { &[] }
}

// Each platform uses its own half: evdev on Linux, scan codes on Windows.
// ponytail: linear scans over ~120 rows per keystroke, a 256-entry lookup table if it ever shows up in a profile
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn hid_from_evdev(code: u16) -> Option<u16> {
    KEYS.iter().find(|k| k.1 == code).map(|k| k.0)
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn evdev_from_hid(hid: u16) -> Option<u16> {
    KEYS.iter().find(|k| k.0 == hid).map(|k| k.1)
}

#[cfg_attr(not(windows), allow(dead_code))]
pub fn hid_from_scan(scan: u16) -> Option<u16> {
    KEYS.iter().find(|k| k.2 == scan).map(|k| k.0)
}

#[cfg_attr(not(windows), allow(dead_code))]
pub fn scan_from_hid(hid: u16) -> Option<u16> {
    KEYS.iter().find(|k| k.0 == hid).map(|k| k.2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables_are_consistent() {
        for (i, &(hid, evdev, scan)) in KEYS.iter().enumerate() {
            assert!(KEYS[..i].iter().all(|k| k.0 != hid), "duplicate HID {hid:#x}");
            assert_eq!(evdev_from_hid(hid), Some(evdev));
            assert_eq!(scan_from_hid(hid), Some(scan));
            // Non-US # shares its key with \ everywhere, so it comes back as \.
            let back = if hid == 0x32 { 0x31 } else { hid };
            assert_eq!(hid_from_evdev(evdev), Some(back), "evdev {evdev}");
            assert_eq!(hid_from_scan(scan), Some(back), "scan {scan:#x}");
        }
        assert_eq!(hid_from_evdev(30), Some(0x04)); // A
        assert_eq!(hid_from_scan(0xE04D), Some(0x4F)); // Right arrow
        assert_eq!(evdev_from_hid(0x66), None); // Power never crosses
        assert_eq!((hid_from_evdev(164), hid_from_scan(0xE022)), (Some(PLAY_PAUSE), Some(PLAY_PAUSE)));
        assert_eq!(swap_modifiers(swap_modifiers(LEFT_CTRL)), LEFT_CTRL);
    }
}
