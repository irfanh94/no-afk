// Tray-only app; no console window on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! no-afk — menu bar shell.
//!
//! Deliberately thin. All keep-awake logic lives in `awake-core` so it can be tested
//! without a UI and reused on Windows/Linux. This file owns the tray,
//! the menu, and the once-a-second tick.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use awake_core::session::{Kind, Manager as AwakeManager, SystemClock};
use awake_core::{default_backend, Flags};
use tauri::image::Image;
use tauri::menu::{
    CheckMenuItem, CheckMenuItemBuilder, MenuBuilder, MenuItem, MenuItemBuilder,
    PredefinedMenuItem, SubmenuBuilder,
};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager as _, Wry};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};
use tauri_plugin_opener::OpenerExt;

const TRAY_ID: &str = "main";
const DONATE_URL: &str = "https://ko-fi.com/irfanhodzic";

/// Durations offered in the menu. `None` = indefinite.
const PRESETS: &[(&str, Option<u64>)] = &[
    ("Indefinitely", None),
    ("For 15 minutes", Some(15 * 60)),
    ("For 30 minutes", Some(30 * 60)),
    ("For 1 hour", Some(60 * 60)),
    ("For 2 hours", Some(2 * 60 * 60)),
    ("For 4 hours", Some(4 * 60 * 60)),
    ("For 8 hours", Some(8 * 60 * 60)),
];

struct MenuHandles {
    status: MenuItem<Wry>,
    toggle: MenuItem<Wry>,
    keep_display: CheckMenuItem<Wry>,
    autostart: CheckMenuItem<Wry>,
}

struct AppState {
    awake: Mutex<AwakeManager>,
    /// Whether to hold the display assertion as well as the system one.
    keep_display: Mutex<bool>,
    menu: Mutex<Option<MenuHandles>>,
    /// Last icon we pushed, so the tick doesn't re-set it 60 times a minute.
    icon_active: Mutex<Option<bool>>,
}

impl AppState {
    fn flags(&self) -> Flags {
        if *self.keep_display.lock().unwrap() {
            Flags::display_and_system()
        } else {
            Flags::system_only()
        }
    }
}

fn tray_icon(active: bool) -> tauri::Result<Image<'static>> {
    let bytes: &[u8] = if active {
        include_bytes!("../icons/tray-active@2x.png")
    } else {
        include_bytes!("../icons/tray-idle@2x.png")
    };
    Image::from_bytes(bytes)
}

/// `8:04:11` / `43:09` / `0:07`
fn format_remaining(d: Duration) -> String {
    let total = d.as_secs();
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// Push current state into the tray icon and menu labels.
fn refresh(app: &AppHandle, state: &AppState) {
    let awake = state.awake.lock().unwrap();
    let active = awake.is_active();

    let status = if !active {
        "Asleep as usual".to_string()
    } else {
        match awake.remaining() {
            Some(left) => format!("Awake — {} left", format_remaining(left)),
            None => "Awake — indefinitely".to_string(),
        }
    };
    drop(awake);

    if let Some(handles) = state.menu.lock().unwrap().as_ref() {
        let _ = handles.status.set_text(status);
        let _ = handles.toggle.set_text(if active { "Turn Off" } else { "Turn On" });
    }

    // Only touch the tray icon when it actually changes.
    let mut last = state.icon_active.lock().unwrap();
    if *last != Some(active) {
        if let (Some(tray), Ok(img)) = (app.tray_by_id(TRAY_ID), tray_icon(active)) {
            let _ = tray.set_icon(Some(img));
            // `set_icon` replaces the underlying NSImage and clears the template
            // flag with it, so it has to be re-applied every time. Without this the
            // glyph renders solid black and is invisible on a dark menu bar.
            #[cfg(target_os = "macos")]
            let _ = tray.set_icon_as_template(true);
        }
        *last = Some(active);
    }
}

fn handle_menu_event(app: &AppHandle, id: &str) {
    let state = app.state::<Arc<AppState>>();

    match id {
        "quit" => {
            // Release explicitly. Tauri may exit the process without unwinding, and a
            // leaked assertion outlives us — the user's Mac would simply never sleep
            // again with no UI left to fix it.
            let _ = state.awake.lock().unwrap().stop();
            app.exit(0);
        }

        "toggle" => {
            let mut awake = state.awake.lock().unwrap();
            let result = if awake.is_active() {
                awake.stop()
            } else {
                awake.start(Kind::Indefinite, state.flags(), "menu toggle")
            };
            if let Err(err) = result {
                eprintln!("no-afk: toggle failed: {err}");
            }
            drop(awake);
            refresh(app, &state);
        }

        "keep_display" => {
            let mut keep = state.keep_display.lock().unwrap();
            *keep = !*keep;
            let now = *keep;
            drop(keep);

            if let Some(handles) = state.menu.lock().unwrap().as_ref() {
                let _ = handles.keep_display.set_checked(now);
            }

            // Re-acquire with the new flags so the change takes effect immediately
            // rather than at the next session.
            let mut awake = state.awake.lock().unwrap();
            if let Some(session) = awake.session().cloned() {
                if let Err(err) = awake.start(session.kind, state.flags(), session.reason) {
                    eprintln!("no-afk: could not re-apply display setting: {err}");
                }
            }
            drop(awake);
            refresh(app, &state);
        }

        "donate" => {
            // Via the opener plugin rather than `open(1)` so this works unchanged on
            // Windows and Linux.
            if let Err(err) = app.opener().open_url(DONATE_URL, None::<&str>) {
                eprintln!("no-afk: could not open {DONATE_URL}: {err}");
            }
        }

        "autostart" => {
            let mgr = app.autolaunch();
            let enabled = mgr.is_enabled().unwrap_or(false);
            let result = if enabled { mgr.disable() } else { mgr.enable() };
            if let Err(err) = result {
                eprintln!("no-afk: autostart toggle failed: {err}");
            }
            if let Some(handles) = state.menu.lock().unwrap().as_ref() {
                let _ = handles.autostart.set_checked(mgr.is_enabled().unwrap_or(false));
            }
        }

        other => {
            // Duration presets are registered as `start:<seconds>` / `start:inf`.
            if let Some(spec) = other.strip_prefix("start:") {
                let kind = match spec {
                    "inf" => Kind::Indefinite,
                    secs => match secs.parse::<u64>() {
                        Ok(s) => Kind::For(Duration::from_secs(s)),
                        Err(_) => return,
                    },
                };
                let mut awake = state.awake.lock().unwrap();
                if let Err(err) = awake.start(kind, state.flags(), "menu") {
                    eprintln!("no-afk: start failed: {err}");
                }
                drop(awake);
                refresh(app, &state);
            }
        }
    }
}

fn main() {
    let state = Arc::new(AppState {
        awake: Mutex::new(AwakeManager::new(default_backend(), Arc::new(SystemClock))),
        keep_display: Mutex::new(true),
        menu: Mutex::new(None),
        icon_active: Mutex::new(None),
    });

    tauri::Builder::default()
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(tauri_plugin_opener::init())
        .manage(state.clone())
        .setup(move |app| {
            // Menu-bar-only: no Dock icon. Matches LSUIElement in Info.plist, but also
            // applies under `tauri dev`, where the bundle plist isn't used.
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            let handle = app.handle();

            let status = MenuItemBuilder::with_id("status", "Asleep as usual")
                .enabled(false)
                .build(app)?;
            let toggle = MenuItemBuilder::with_id("toggle", "Turn On").build(app)?;

            let mut presets = SubmenuBuilder::new(app, "Stay awake…");
            for (label, secs) in PRESETS {
                let id = match secs {
                    None => "start:inf".to_string(),
                    Some(s) => format!("start:{s}"),
                };
                presets = presets.item(&MenuItemBuilder::with_id(id, *label).build(app)?);
            }
            let presets = presets.build()?;

            let keep_display = CheckMenuItemBuilder::with_id("keep_display", "Keep display on")
                .checked(true)
                .build(app)?;

            let autostart_on = handle.autolaunch().is_enabled().unwrap_or(false);
            let autostart = CheckMenuItemBuilder::with_id("autostart", "Launch at login")
                .checked(autostart_on)
                .build(app)?;

            let donate = MenuItemBuilder::with_id("donate", "Buy me a coffee").build(app)?;

            let quit = MenuItemBuilder::with_id("quit", "Quit no-afk")
                .accelerator("CmdOrCtrl+Q")
                .build(app)?;

            let menu = MenuBuilder::new(app)
                .items(&[
                    &status,
                    &PredefinedMenuItem::separator(app)?,
                    &toggle,
                    &presets,
                    &PredefinedMenuItem::separator(app)?,
                    &keep_display,
                    &autostart,
                    &PredefinedMenuItem::separator(app)?,
                    &donate,
                    &PredefinedMenuItem::separator(app)?,
                    &quit,
                ])
                .build()?;

            *state.menu.lock().unwrap() = Some(MenuHandles {
                status,
                toggle,
                keep_display,
                autostart,
            });

            TrayIconBuilder::with_id(TRAY_ID)
                .icon(tray_icon(false)?)
                .icon_as_template(true)
                .menu(&menu)
                .show_menu_on_left_click(true)
                .on_menu_event(|app, event| handle_menu_event(app, event.id().as_ref()))
                .build(app)?;

            // Drive countdown + auto-expiry.
            let app_handle = handle.clone();
            let tick_state = state.clone();
            std::thread::spawn(move || loop {
                std::thread::sleep(Duration::from_secs(1));

                let ended = {
                    let mut awake = tick_state.awake.lock().unwrap();
                    awake.tick().unwrap_or(false)
                };

                refresh(&app_handle, &tick_state);

                if ended {
                    println!("no-afk: session ended");
                }
            });

            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("failed to start no-afk")
        .run(|app, event| {
            // Belt-and-braces: release on any exit path, not just the Quit item.
            if let tauri::RunEvent::ExitRequested { .. } | tauri::RunEvent::Exit = event {
                let state = app.state::<Arc<AppState>>();
                let _ = state.awake.lock().unwrap().stop();
            }
        });
}
