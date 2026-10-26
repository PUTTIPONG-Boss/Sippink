//! Tray icon and its menu.
//!
//! While the window is hidden eframe runs no frames, so anything that waits
//! for `update()` never happens: tray clicks and menu picks used to sit in a
//! queue nobody read. Their handlers therefore act right away on the thread
//! that owns the tray (the UI thread): Quit removes the icon and exits, and
//! Open shows the window through Win32 so frames start again, leaving a
//! [`Show`] request for the next `update()` to size it.

use std::cell::RefCell;
use std::sync::{Arc, Mutex};

use tray_icon::menu::{Menu, MenuEvent, MenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

/// What the tray asked the window to do.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Show {
    Compact,
    Full,
}

/// Set by the tray handlers, taken by `update()`.
pub type Request = Arc<Mutex<Option<Show>>>;

thread_local! {
    // Dropping the TrayIcon removes it from the taskbar, so Quit can take it
    // out before exiting instead of leaving a ghost icon behind.
    static TRAY: RefCell<Option<TrayIcon>> = const { RefCell::new(None) };
}

/// Creates the tray icon and wires its events. `hwnd` is the main window
/// (0 if unknown).
pub fn install(icon: Icon, ctx: egui::Context, hwnd: isize) -> Request {
    let menu = Menu::new();
    let open = MenuItem::new("Open full window", true, None);
    let quit = MenuItem::new("Quit", true, None);
    menu.append(&open).unwrap();
    menu.append(&quit).unwrap();
    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        // Left click opens the window, right click the menu.
        .with_menu_on_left_click(false)
        .with_tooltip("Sippink — sip a link. Click to paste one")
        .with_icon(icon)
        .build()
        .expect("tray");
    TRAY.with(|t| *t.borrow_mut() = Some(tray));

    let request: Request = Arc::new(Mutex::new(None));
    let ask = {
        let request = request.clone();
        move |what: Show| {
            *request.lock().unwrap() = Some(what);
            show_window(hwnd);
            ctx.request_repaint();
        }
    };

    let (open_id, quit_id) = (open.id().clone(), quit.id().clone());
    let ask_menu = ask.clone();
    MenuEvent::set_event_handler(Some(move |ev: MenuEvent| {
        crate::dbg_log(&format!("menu {:?}", ev.id));
        if ev.id == quit_id {
            quit_now();
        } else if ev.id == open_id {
            ask_menu(Show::Full);
        }
    }));
    TrayIconEvent::set_event_handler(Some(move |ev: TrayIconEvent| match ev {
        TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. }
        | TrayIconEvent::DoubleClick { button: MouseButton::Left, .. } => {
            crate::dbg_log("tray click");
            ask(Show::Compact);
        }
        _ => {}
    }));
    request
}

/// Removes the tray icon (when called on the UI thread) and ends the app.
pub fn quit_now() -> ! {
    crate::dbg_log("quit");
    let _ = TRAY.try_with(|t| t.borrow_mut().take());
    std::process::exit(0)
}

#[cfg(windows)]
fn show_window(hwnd: isize) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{SetForegroundWindow, ShowWindow, SW_RESTORE, SW_SHOW};
    if hwnd == 0 {
        return;
    }
    // SAFETY: plain Win32 calls on our own top-level window handle.
    unsafe {
        ShowWindow(hwnd, SW_SHOW);
        ShowWindow(hwnd, SW_RESTORE);
        SetForegroundWindow(hwnd);
    }
}

#[cfg(not(windows))]
fn show_window(_hwnd: isize) {}
