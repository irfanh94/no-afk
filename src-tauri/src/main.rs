// Tray-only app; no console window on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! no-afk — menu bar shell.
//!
//! Deliberately thin. All keep-awake logic lives in `awake-core` so it can be tested
//! without a UI and reused on Windows/Linux. This file owns the tray, the menu, the
//! settings window and the once-a-second tick.

mod settings;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use awake_core::session::{Kind, Manager as AwakeManager, SystemClock};
use awake_core::{default_backend, Backend, Flags};
use serde::Serialize;
use settings::Settings;
use tauri::image::Image;
use tauri::menu::{
    Menu, MenuBuilder, MenuItem, MenuItemBuilder, PredefinedMenuItem, SubmenuBuilder,
};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager as _, State, Wry};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};
use tauri_plugin_opener::OpenerExt;
use tauri_plugin_updater::UpdaterExt;

const TRAY_ID: &str = "main";
const SETTINGS_WINDOW: &str = "settings";
const DONATE_URL: &str = "https://ko-fi.com/irfanhodzic";

/// Set only by the Quit menu item.
///
/// A tray app must survive its last window closing, so `ExitRequested` is normally
/// vetoed. But vetoing it unconditionally would make the app unquittable, so real
/// quits are distinguished by this flag.
static QUIT_REQUESTED: AtomicBool = AtomicBool::new(false);

/// Durations offered in the tray submenu and the settings dropdown. `None` =
/// indefinite.
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
}

struct AppState {
    /// Kept alongside the manager so commands can query system-wide assertions
    /// without reaching through it.
    backend: Arc<dyn Backend>,
    awake: Mutex<AwakeManager>,
    settings: Mutex<Settings>,
    menu: Mutex<Option<MenuHandles>>,
    /// Last icon we pushed, so the tick doesn't re-set it 60 times a minute.
    icon_active: Mutex<Option<bool>>,
    /// Version string of a newer release, once a check has found one.
    ///
    /// Populated by a background check at startup. Without that, the only way a user
    /// would ever learn about an update is by opening Settings and pressing a button,
    /// which almost nobody does.
    available_update: Mutex<Option<String>>,
}

impl AppState {
    fn flags(&self) -> Flags {
        if self.settings.lock().unwrap().keep_display {
            Flags::display_and_system()
        } else {
            Flags::system_only()
        }
    }

    fn default_kind(&self) -> Kind {
        match self.settings.lock().unwrap().default_duration_secs {
            Some(secs) => Kind::For(Duration::from_secs(secs)),
            None => Kind::Indefinite,
        }
    }
}

// ---------------------------------------------------------------------------
// DTOs
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct StatusDto {
    active: bool,
    /// `None` when inactive *or* indefinite; pair with `indefinite` to tell them apart.
    remaining_secs: Option<u64>,
    indefinite: bool,
    keep_display: bool,
}

#[derive(Serialize)]
struct AssertionDto {
    pid: i32,
    process: String,
    kind: String,
    name: String,
    /// Whether this is one of ours, so the UI can highlight it.
    ours: bool,
}

#[derive(Serialize)]
struct PresetDto {
    label: String,
    secs: Option<u64>,
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

#[tauri::command]
fn get_settings(state: State<'_, Arc<AppState>>) -> Settings {
    state.settings.lock().unwrap().clone()
}

#[tauri::command]
fn set_settings(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    new: Settings,
) -> Result<(), String> {
    *state.settings.lock().unwrap() = new.clone();
    new.save(&app)?;

    // Re-acquire so a flag change takes effect on the *current* session rather than
    // silently waiting for the next one.
    let flags = state.flags();
    let mut awake = state.awake.lock().unwrap();
    if let Some(session) = awake.session().cloned() {
        awake
            .start(session.kind, flags, session.reason)
            .map_err(|e| e.to_string())?;
    }
    drop(awake);

    refresh(&app, &state);
    Ok(())
}

#[tauri::command]
fn get_status(state: State<'_, Arc<AppState>>) -> StatusDto {
    let awake = state.awake.lock().unwrap();
    let active = awake.is_active();
    let indefinite = matches!(awake.session().map(|s| s.kind), Some(Kind::Indefinite));

    StatusDto {
        active,
        remaining_secs: awake.remaining().map(|d| d.as_secs()),
        indefinite,
        keep_display: state.settings.lock().unwrap().keep_display,
    }
}

#[tauri::command]
fn start_session(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    secs: Option<u64>,
) -> Result<(), String> {
    let kind = match secs {
        Some(s) => Kind::For(Duration::from_secs(s)),
        None => Kind::Indefinite,
    };
    let flags = state.flags();
    state
        .awake
        .lock()
        .unwrap()
        .start(kind, flags, "settings window")
        .map_err(|e| e.to_string())?;

    refresh(&app, &state);
    Ok(())
}

#[tauri::command]
fn stop_session(app: AppHandle, state: State<'_, Arc<AppState>>) -> Result<(), String> {
    state
        .awake
        .lock()
        .unwrap()
        .stop()
        .map_err(|e| e.to_string())?;
    refresh(&app, &state);
    Ok(())
}

/// Every assertion held on the system, by any process — the "why is my Mac awake?"
/// panel. Sorted so display-blocking ones come first, since those are what people
/// are usually hunting for.
#[tauri::command]
fn list_assertions(state: State<'_, Arc<AppState>>) -> Result<Vec<AssertionDto>, String> {
    let me = std::process::id() as i32;
    let mut list: Vec<AssertionDto> = state
        .backend
        .system_assertions()
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|a| AssertionDto {
            ours: a.pid == me,
            pid: a.pid,
            process: a.process,
            kind: a.kind,
            name: a.name,
        })
        .collect();

    list.sort_by(|a, b| {
        let rank = |d: &AssertionDto| match () {
            _ if d.ours => 0,
            _ if d.kind.contains("Display") => 1,
            _ if d.kind.contains("Idle") || d.kind.contains("System") => 2,
            _ => 3,
        };
        rank(a)
            .cmp(&rank(b))
            .then_with(|| a.process.cmp(&b.process))
    });
    Ok(list)
}

#[tauri::command]
fn presets() -> Vec<PresetDto> {
    PRESETS
        .iter()
        .map(|(label, secs)| PresetDto {
            label: (*label).to_string(),
            secs: *secs,
        })
        .collect()
}

#[tauri::command]
fn get_autostart(app: AppHandle) -> bool {
    app.autolaunch().is_enabled().unwrap_or(false)
}

#[tauri::command]
fn set_autostart(app: AppHandle, enabled: bool) -> Result<(), String> {
    let mgr = app.autolaunch();
    if enabled {
        mgr.enable().map_err(|e| e.to_string())
    } else {
        mgr.disable().map_err(|e| e.to_string())
    }
}

#[tauri::command]
fn open_donate(app: AppHandle) -> Result<(), String> {
    app.opener()
        .open_url(DONATE_URL, None::<&str>)
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn app_version(app: AppHandle) -> String {
    app.package_info().version.to_string()
}

// ---------------------------------------------------------------------------
// Updates
// ---------------------------------------------------------------------------

/// Ask the update endpoint whether a newer version exists.
///
/// `Ok(None)` means up to date. An `Err` is expected and normal before the first
/// release exists, since the manifest 404s until then.
async fn look_for_update(app: &AppHandle) -> Result<Option<String>, String> {
    let updater = app.updater().map_err(|e| e.to_string())?;
    match updater.check().await {
        Ok(Some(update)) => Ok(Some(update.version.clone())),
        Ok(None) => Ok(None),
        Err(err) => Err(err.to_string()),
    }
}

#[tauri::command]
async fn check_for_update(app: AppHandle) -> Result<Option<String>, String> {
    let found = look_for_update(&app).await?;
    *app.state::<Arc<AppState>>()
        .available_update
        .lock()
        .unwrap() = found.clone();
    Ok(found)
}

/// What a background check already found, without hitting the network again.
#[tauri::command]
fn pending_update(state: State<'_, Arc<AppState>>) -> Option<String> {
    state.available_update.lock().unwrap().clone()
}

#[tauri::command]
async fn install_update(app: AppHandle) -> Result<(), String> {
    let updater = app.updater().map_err(|e| e.to_string())?;
    let Some(update) = updater.check().await.map_err(|e| e.to_string())? else {
        return Err("no update available".into());
    };

    update
        .download_and_install(|_chunk, _total| {}, || {})
        .await
        .map_err(|e| e.to_string())?;

    // Release the session before the process is replaced, so the new build starts
    // from a clean slate rather than inheriting a stale tray state.
    let _ = app.state::<Arc<AppState>>().awake.lock().unwrap().stop();

    app.restart()
}

// ---------------------------------------------------------------------------
// Tray
// ---------------------------------------------------------------------------

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
        let _ = handles
            .toggle
            .set_text(if active { "Turn Off" } else { "Turn On" });
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

fn open_settings(app: &AppHandle) {
    // Reuse the existing window rather than stacking duplicates.
    if let Some(win) = app.get_webview_window(SETTINGS_WINDOW) {
        let _ = win.show();
        let _ = win.unminimize();
        let _ = win.set_focus();
        return;
    }

    let built = tauri::WebviewWindowBuilder::new(
        app,
        SETTINGS_WINDOW,
        tauri::WebviewUrl::App("index.html".into()),
    )
    .title("no-afk")
    // Tall enough that every section fits without scrolling on a default display.
    .inner_size(540.0, 850.0)
    .min_inner_size(480.0, 520.0)
    .resizable(true)
    .build();

    match built {
        Ok(win) => {
            let _ = win.set_focus();
        }
        Err(err) => eprintln!("no-afk: could not open settings: {err}"),
    }
}

fn build_menu(app: &AppHandle) -> tauri::Result<(Menu<Wry>, MenuHandles)> {
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

    let settings_item = MenuItemBuilder::with_id("settings", "Settings…")
        .accelerator("CmdOrCtrl+,")
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
            &settings_item,
            &donate,
            &PredefinedMenuItem::separator(app)?,
            &quit,
        ])
        .build()?;

    Ok((menu, MenuHandles { status, toggle }))
}

fn handle_menu_event(app: &AppHandle, id: &str) {
    let state = app.state::<Arc<AppState>>();

    match id {
        "quit" => {
            QUIT_REQUESTED.store(true, Ordering::SeqCst);
            // Release explicitly. Tauri may exit the process without unwinding, and a
            // leaked assertion outlives us — the user's Mac would simply never sleep
            // again with no UI left to fix it.
            let _ = state.awake.lock().unwrap().stop();
            app.exit(0);
        }

        "settings" => open_settings(app),

        "donate" => {
            if let Err(err) = app.opener().open_url(DONATE_URL, None::<&str>) {
                eprintln!("no-afk: could not open {DONATE_URL}: {err}");
            }
        }

        "toggle" => {
            let flags = state.flags();
            let kind = state.default_kind();
            let mut awake = state.awake.lock().unwrap();

            let result = if awake.is_active() {
                awake.stop()
            } else {
                awake.start(kind, flags, "menu toggle")
            };
            if let Err(err) = result {
                eprintln!("no-afk: toggle failed: {err}");
            }
            drop(awake);
            refresh(app, &state);
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
                let flags = state.flags();
                if let Err(err) = state.awake.lock().unwrap().start(kind, flags, "menu") {
                    eprintln!("no-afk: start failed: {err}");
                }
                refresh(app, &state);
            }
        }
    }
}

fn main() {
    let backend = default_backend();
    let state = Arc::new(AppState {
        backend: Arc::clone(&backend),
        awake: Mutex::new(AwakeManager::new(backend, Arc::new(SystemClock))),
        settings: Mutex::new(Settings::default()),
        menu: Mutex::new(None),
        icon_active: Mutex::new(None),
        available_update: Mutex::new(None),
    });

    tauri::Builder::default()
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(state.clone())
        .invoke_handler(tauri::generate_handler![
            get_settings,
            set_settings,
            get_status,
            start_session,
            stop_session,
            list_assertions,
            presets,
            get_autostart,
            set_autostart,
            open_donate,
            app_version,
            check_for_update,
            pending_update,
            install_update,
        ])
        .setup(move |app| {
            // Menu-bar-only: no Dock icon. Matches LSUIElement in Info.plist, but also
            // applies under `tauri dev`, where the bundle plist isn't used.
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            let handle = app.handle().clone();
            *state.settings.lock().unwrap() = Settings::load(&handle);

            let (menu, handles) = build_menu(&handle)?;
            *state.menu.lock().unwrap() = Some(handles);

            TrayIconBuilder::with_id(TRAY_ID)
                .icon(tray_icon(false)?)
                .icon_as_template(true)
                .menu(&menu)
                .show_menu_on_left_click(true)
                .on_menu_event(|app, event| handle_menu_event(app, event.id().as_ref()))
                .build(app)?;

            // Cold-start route into the settings window, for development and for
            // `open -a no-afk --args --settings` on a *fresh* launch. Once an instance
            // exists macOS drops the args, so the reopen handler in `run` below is
            // what covers the common case.
            if std::env::args().any(|a| a == "--settings") {
                open_settings(&handle);
            }

            // Look for an update once, in the background. Failures are silent: before
            // the first release the manifest 404s, and a user who never opens Settings
            // should not be shown network errors from a tray app.
            let update_handle = handle.clone();
            let update_state = state.clone();
            tauri::async_runtime::spawn(async move {
                match look_for_update(&update_handle).await {
                    Ok(Some(version)) => {
                        println!("no-afk: update available: {version}");
                        *update_state.available_update.lock().unwrap() = Some(version);
                    }
                    Ok(None) => {}
                    Err(err) => eprintln!("no-afk: update check failed: {err}"),
                }
            });

            // Drive countdown + auto-expiry.
            let tick_handle = handle.clone();
            let tick_state = state.clone();
            std::thread::spawn(move || loop {
                std::thread::sleep(Duration::from_secs(1));

                let ended = {
                    let mut awake = tick_state.awake.lock().unwrap();
                    awake.tick().unwrap_or(false)
                };

                refresh(&tick_handle, &tick_state);

                if ended {
                    println!("no-afk: session ended");
                }
            });

            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("failed to start no-afk")
        .run(|app, event| match event {
            // Closing the settings window must not quit a tray app — but a real Quit
            // must still get through.
            tauri::RunEvent::ExitRequested { api, .. } => {
                if !QUIT_REQUESTED.load(Ordering::SeqCst) {
                    api.prevent_exit();
                }
            }

            // Launching the app again while it is already running — double-clicking it
            // in Finder, or picking it from Spotlight. macOS delivers a reopen event
            // and *discards* any --args, so this is the only route that works once an
            // instance exists. Without it, re-launching a tray app appears to do
            // nothing at all.
            #[cfg(target_os = "macos")]
            tauri::RunEvent::Reopen { .. } => open_settings(app),

            tauri::RunEvent::Exit => {
                let state = app.state::<Arc<AppState>>();
                let _ = state.awake.lock().unwrap().stop();
            }
            _ => {}
        });
}
