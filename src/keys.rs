//! Ctrl+V / C / X / A / Z / Y with a non-Latin keyboard layout.
//!
//! egui-winit 0.27 recognises shortcuts by the character a key types. With
//! the Thai layout the V key types "อ", which egui can't name, so it drops
//! the whole key event: Ctrl+V did nothing and a link couldn't be pasted
//! (it only seemed tied to the tray because Windows can switch the layout
//! per window). Windows still reports the key's position (VK_V) in
//! WM_KEYDOWN, so a winit message hook records those presses and [`apply`]
//! adds the events egui would have produced on an English layout.

use std::sync::Mutex;

use egui::{Event, Key, Modifiers};

static PRESSED: Mutex<Vec<Key>> = Mutex::new(Vec::new());

/// winit message hook (runs before winit sees each message). Records
/// Ctrl+letter presses by key position; never swallows a message.
pub fn hook(msg: *const std::ffi::c_void) -> bool {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_CONTROL, VK_MENU};
        use windows_sys::Win32::UI::WindowsAndMessaging::{MSG, WM_KEYDOWN};
        // SAFETY: winit hands the hook a valid MSG pointer for this call.
        let m = unsafe { &*(msg as *const MSG) };
        // Ctrl without Alt: AltGr arrives as Ctrl+Alt and types characters.
        let ctrl = unsafe { GetKeyState(VK_CONTROL as i32) < 0 && GetKeyState(VK_MENU as i32) >= 0 };
        if m.message == WM_KEYDOWN && ctrl {
            let key = match m.wParam as u8 {
                b'V' => Some(Key::V),
                b'C' => Some(Key::C),
                b'X' => Some(Key::X),
                b'A' => Some(Key::A),
                b'Z' => Some(Key::Z),
                b'Y' => Some(Key::Y),
                _ => None,
            };
            if let Some(key) = key {
                PRESSED.lock().unwrap().push(key);
            }
        }
    }
    let _ = msg;
    false
}

/// Adds the shortcuts egui missed to this frame's input. Presses egui did
/// understand (English layout) are left alone so nothing happens twice.
pub fn apply(raw: &mut egui::RawInput) {
    let pressed = std::mem::take(&mut *PRESSED.lock().unwrap());
    for key in pressed {
        if understood(&raw.events, key) {
            continue;
        }
        match key {
            Key::V => {
                if let Some(text) = clipboard_text() {
                    raw.events.push(Event::Paste(text));
                }
            }
            Key::C => raw.events.push(Event::Copy),
            Key::X => raw.events.push(Event::Cut),
            // Select all / undo / redo are plain key events with Ctrl held.
            key => {
                let modifiers = Modifiers { ctrl: true, command: true, ..Default::default() };
                for pressed in [true, false] {
                    raw.events.push(Event::Key { key, physical_key: Some(key), pressed, repeat: false, modifiers });
                }
            }
        }
    }
}

fn understood(events: &[Event], key: Key) -> bool {
    events.iter().any(|e| match (e, key) {
        (Event::Paste(_), Key::V) | (Event::Copy, Key::C) | (Event::Cut, Key::X) => true,
        (Event::Key { key: k, pressed: true, .. }, _) => *k == key,
        _ => false,
    })
}

fn clipboard_text() -> Option<String> {
    let text = arboard::Clipboard::new().ok()?.get_text().ok()?;
    let text = text.replace("\r\n", "\n");
    (!text.is_empty()).then_some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leaves_shortcuts_egui_understood() {
        let english = vec![Event::Copy];
        assert!(understood(&english, Key::C));
        assert!(!understood(&english, Key::V));
        let thai: Vec<Event> = vec![Event::Text("x".into())];
        assert!(!understood(&thai, Key::C));
    }

    #[test]
    fn adds_select_all_as_key_press_and_release() {
        PRESSED.lock().unwrap().push(Key::A);
        let mut raw = egui::RawInput::default();
        apply(&mut raw);
        let keys: Vec<(Key, bool)> = raw
            .events
            .iter()
            .filter_map(|e| match e {
                Event::Key { key, pressed, modifiers, .. } if modifiers.command => Some((*key, *pressed)),
                _ => None,
            })
            .collect();
        assert_eq!(keys, vec![(Key::A, true), (Key::A, false)]);
    }
}
