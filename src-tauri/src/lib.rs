mod controller;
mod instance;
mod platform;
mod updater;

use controller::{
    ControllerState, DeviceMutationGuard, MutationCoordinator, SharedController, DEFAULT_SHORTCUT,
};
use instance::InstanceGuard;
use std::error::Error;
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Manager, Runtime, State, WindowEvent};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

const APP_IDENTIFIER: &str = "io.github.wakemeup0.controwly";
const CONTROLLER_EVENT: &str = "controller-state";
const CLOSE_REQUESTED_EVENT: &str = "close-requested";

#[derive(Default)]
struct RuntimeState {
    tray_available: AtomicBool,
}

pub fn run() -> i32 {
    let data_dir = match default_data_dir() {
        Ok(path) => path,
        Err(error) => {
            show_fatal_error(&format!(
                "Controwly cannot determine its data directory: {error}"
            ));
            return 2;
        }
    };

    let builder = tauri::Builder::default()
        // This plugin is deliberately first: a second GUI launch is forwarded
        // to the existing window before our native lock is acquired in setup.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, _shortcut, event| {
                    if event.state == ShortcutState::Pressed {
                        handle_shortcut(app);
                    }
                })
                .build(),
        )
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(move |app| {
            // The native guard is acquired before reading recovery state,
            // creating the coordinator, or registering tray/shortcut hooks.
            let instance = InstanceGuard::acquire(&data_dir)
                .map_err(|error| -> Box<dyn Error> { Box::new(error) })?;
            app.manage(instance);
            let app_data_dir = app
                .path()
                .app_data_dir()
                .unwrap_or_else(|_| data_dir.clone());
            let controller = MutationCoordinator::new(platform::create(), app_data_dir)
                .map_err(|error| -> Box<dyn Error> { boxed_error(error) })?;
            let runtime = Arc::new(RuntimeState::default());
            app.manage(controller.clone());
            app.manage(runtime.clone());
            let guard = tauri::async_runtime::block_on(controller.acquire_exclusive());
            register_shortcut(app.handle(), &guard);
            setup_tray(app, controller.clone(), runtime.clone(), &guard);
            drop(guard);
            app.manage(updater::UpdaterState::new(controller));
            updater::spawn_startup_check(app.handle());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_state,
            set_selected,
            set_device_enabled,
            set_selected_enabled,
            restore_disabled,
            open_bluetooth_settings,
            quit_keep_state,
            restore_and_quit,
            updater::get_update_state,
            updater::check_for_updates,
            updater::install_update,
        ])
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                let runtime = window.state::<Arc<RuntimeState>>();
                if runtime.tray_available.load(Ordering::Acquire) {
                    api.prevent_close();
                    let _ = window.hide();
                } else {
                    // Without a functioning tray, leave the window available
                    // and ask the UI to present explicit restore/keep choices.
                    api.prevent_close();
                    let _ = window.app_handle().emit(CLOSE_REQUESTED_EVENT, ());
                }
            }
        });

    if let Err(error) = builder.run(tauri::generate_context!()) {
        show_fatal_error(&format!(
            "Controwly terminated with an application error: {error}"
        ));
        return 3;
    }
    0
}

pub fn run_restore_cli() -> i32 {
    let data_dir = match default_data_dir() {
        Ok(path) => path,
        Err(error) => {
            eprintln!("Controwly restore failed: cannot determine data directory: {error}");
            return 2;
        }
    };
    let _instance = match InstanceGuard::acquire(&data_dir) {
        Ok(guard) => guard,
        Err(error) => {
            eprintln!("Controwly restore failed: {error}");
            return 3;
        }
    };
    let controller = match MutationCoordinator::new(platform::create(), data_dir) {
        Ok(controller) => controller,
        Err(error) => {
            eprintln!("Controwly restore failed: {error}");
            return 4;
        }
    };
    let result = tauri::async_runtime::block_on(async {
        let guard = controller.acquire_exclusive().await;
        guard.restore_for_shutdown()
    });
    match result {
        Ok(summary) => {
            eprintln!(
                "Controwly restore completed: {} device record(s) restored",
                summary.succeeded
            );
            0
        }
        Err(error) => {
            eprintln!("Controwly restore failed: {error}");
            5
        }
    }
}

#[tauri::command]
async fn get_state<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, SharedController>,
) -> Result<ControllerState, String> {
    let snapshot = state.inner().get_state().await;
    emit_controller_state(&app, &snapshot);
    Ok(snapshot)
}

#[tauri::command]
async fn set_selected<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, SharedController>,
    id: String,
    selected: bool,
) -> Result<ControllerState, String> {
    let controller = state.inner().clone();
    match controller.set_selected(id, selected).await {
        Ok(snapshot) => {
            emit_controller_state(&app, &snapshot);
            Ok(snapshot)
        }
        Err(error) => {
            emit_current_state(&app, &controller).await;
            Err(error)
        }
    }
}

#[tauri::command]
async fn set_device_enabled<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, SharedController>,
    id: String,
    enabled: bool,
) -> Result<ControllerState, String> {
    let controller = state.inner().clone();
    match controller.set_device_enabled(id, enabled).await {
        Ok(snapshot) => {
            emit_controller_state(&app, &snapshot);
            Ok(snapshot)
        }
        Err(error) => {
            emit_current_state(&app, &controller).await;
            Err(error)
        }
    }
}

#[tauri::command]
async fn set_selected_enabled<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, SharedController>,
    enabled: bool,
) -> Result<ControllerState, String> {
    let controller = state.inner().clone();
    match controller.set_selected_enabled(enabled).await {
        Ok(snapshot) => {
            emit_controller_state(&app, &snapshot);
            Ok(snapshot)
        }
        Err(error) => {
            emit_current_state(&app, &controller).await;
            Err(error)
        }
    }
}

#[tauri::command]
async fn restore_disabled<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, SharedController>,
) -> Result<ControllerState, String> {
    let controller = state.inner().clone();
    let snapshot = controller.restore_disabled().await?;
    emit_controller_state(&app, &snapshot);
    Ok(snapshot)
}

#[tauri::command]
async fn open_bluetooth_settings<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, SharedController>,
) -> Result<(), String> {
    let controller = state.inner().clone();
    match controller.open_bluetooth_settings().await {
        Ok(()) => Ok(()),
        Err(error) => {
            emit_current_state(&app, &controller).await;
            Err(error)
        }
    }
}

#[tauri::command]
async fn quit_keep_state<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, SharedController>,
) -> Result<(), String> {
    let controller = state.inner().clone();
    let _guard = controller.acquire_exclusive().await;
    // Explicit keep-state quit is a deliberate process handoff. Exit
    // immediately after acquiring the same gate used by all mutations and
    // retain the guard until the event loop has exited.
    app.exit(0);
    std::future::pending::<()>().await;
    unreachable!("the Tauri process should exit after quit_keep_state")
}

#[tauri::command]
async fn restore_and_quit<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, SharedController>,
) -> Result<(), String> {
    let controller = state.inner().clone();
    let guard = controller.acquire_exclusive().await;
    let (guard, result) = tokio::task::spawn_blocking(move || {
        let result = guard.restore_for_shutdown();
        (guard, result)
    })
    .await
    .map_err(|error| format!("controller worker terminated during restore: {error}"))?;
    match result {
        Ok(_) => {
            app.exit(0);
            std::future::pending::<()>().await;
            drop(guard);
            unreachable!("the Tauri process should exit after restore_and_quit")
        }
        Err(error) => {
            drop(guard);
            emit_current_state(&app, &controller).await;
            Err(error)
        }
    }
}

fn register_shortcut<R: Runtime>(app: &AppHandle<R>, guard: &DeviceMutationGuard<'_>) {
    match app.global_shortcut().register(DEFAULT_SHORTCUT) {
        Ok(()) => guard.set_shortcut_available(true, None),
        Err(error) => guard.set_shortcut_available(
            false,
            Some(format!(
                "Global shortcut {DEFAULT_SHORTCUT} is unavailable: {error}. Choose another application or use the buttons."
            )),
        ),
    }
}

fn setup_tray<R: Runtime>(
    app: &mut tauri::App<R>,
    controller: SharedController,
    runtime: Arc<RuntimeState>,
    guard: &DeviceMutationGuard<'_>,
) {
    let build_result = (|| -> Result<(), Box<dyn std::error::Error>> {
        let show = MenuItem::with_id(app, "show", "Show", true, None::<&str>)?;
        let enable = MenuItem::with_id(
            app,
            "enable-selected",
            "Enable selected",
            true,
            None::<&str>,
        )?;
        let disable = MenuItem::with_id(
            app,
            "disable-selected",
            "Disable selected",
            true,
            None::<&str>,
        )?;
        let restore = MenuItem::with_id(
            app,
            "restore-disabled",
            "Restore Controwly-disabled devices",
            true,
            None::<&str>,
        )?;
        let restore_quit = MenuItem::with_id(
            app,
            "restore-and-quit",
            "Restore and quit",
            true,
            None::<&str>,
        )?;
        let quit = MenuItem::with_id(
            app,
            "quit-keep-state",
            "Quit (keep device state)",
            true,
            None::<&str>,
        )?;
        let menu = Menu::with_items(
            app,
            &[&show, &enable, &disable, &restore, &restore_quit, &quit],
        )?;
        let controller_for_menu = controller.clone();
        TrayIconBuilder::new()
            .menu(&menu)
            .show_menu_on_left_click(false)
            .on_menu_event(move |app, event| {
                let id = event.id().as_ref().to_owned();
                match id.as_str() {
                    "show" => {
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                    }
                    "enable-selected" => {
                        spawn_selected_action(app, controller_for_menu.clone(), true);
                    }
                    "disable-selected" => {
                        spawn_selected_action(app, controller_for_menu.clone(), false);
                    }
                    "restore-disabled" => {
                        spawn_restore_action(app, controller_for_menu.clone(), false);
                    }
                    "restore-and-quit" => {
                        spawn_restore_action(app, controller_for_menu.clone(), true);
                    }
                    "quit-keep-state" => spawn_quit_action(app, controller_for_menu.clone()),
                    _ => {}
                }
            })
            .build(app)?;
        runtime.tray_available.store(true, Ordering::Release);
        Ok(())
    })();
    if let Err(error) = build_result {
        runtime.tray_available.store(false, Ordering::Release);
        guard.record_error(format!(
            "System tray is unavailable: {error}. The window will stay open on close; use explicit restore or keep-state quit."
        ));
    }
}

fn spawn_selected_action<R: Runtime>(
    app: &AppHandle<R>,
    controller: SharedController,
    enabled: bool,
) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        match controller.set_selected_enabled(enabled).await {
            Ok(snapshot) => emit_controller_state(&app, &snapshot),
            Err(error) => {
                let _ = app.emit("controller-error", error);
                emit_current_state(&app, &controller).await;
            }
        }
    });
}

fn spawn_restore_action<R: Runtime>(
    app: &AppHandle<R>,
    controller: SharedController,
    quit_after: bool,
) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        if quit_after {
            let guard = controller.acquire_exclusive().await;
            let worker = tokio::task::spawn_blocking(move || {
                let result = guard.restore_for_shutdown();
                (guard, result)
            })
            .await;
            match worker {
                Ok((guard, Ok(_))) => {
                    app.exit(0);
                    std::future::pending::<()>().await;
                    drop(guard);
                    unreachable!("the Tauri process should exit after tray restore");
                }
                Ok((guard, Err(error))) => {
                    drop(guard);
                    let _ = app.emit("controller-error", error);
                    emit_current_state(&app, &controller).await;
                }
                Err(error) => {
                    let _ = app.emit(
                        "controller-error",
                        format!("controller worker terminated during restore: {error}"),
                    );
                    emit_current_state(&app, &controller).await;
                }
            }
            return;
        }
        match controller.restore_disabled().await {
            Ok(snapshot) => emit_controller_state(&app, &snapshot),
            Err(error) => {
                let _ = app.emit("controller-error", error);
                emit_current_state(&app, &controller).await;
            }
        }
    });
}

fn spawn_quit_action<R: Runtime>(app: &AppHandle<R>, controller: SharedController) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let _guard = controller.acquire_exclusive().await;
        app.exit(0);
        std::future::pending::<()>().await;
        unreachable!("the Tauri process should exit after tray quit");
    });
}

fn handle_shortcut<R: Runtime>(app: &AppHandle<R>) {
    let controller = app.state::<SharedController>().inner().clone();
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let snapshot = controller.get_state().await;
        let selected: Vec<_> = snapshot
            .devices
            .iter()
            .filter(|device| device.selected)
            .collect();
        let enabled = !selected.iter().any(|device| device.enabled);
        match controller.set_selected_enabled(enabled).await {
            Ok(next) => emit_controller_state(&app, &next),
            Err(error) => {
                let _ = app.emit("controller-error", error);
                emit_current_state(&app, &controller).await;
            }
        }
    });
}

async fn emit_current_state<R: Runtime>(app: &AppHandle<R>, controller: &SharedController) {
    let snapshot = controller.get_state().await;
    emit_controller_state(app, &snapshot);
}

fn emit_controller_state<R: Runtime>(app: &AppHandle<R>, state: &ControllerState) {
    let _ = app.emit(CONTROLLER_EVENT, state);
}

fn default_data_dir() -> Result<PathBuf, String> {
    #[cfg(windows)]
    {
        if let Some(value) = std::env::var_os("APPDATA") {
            return Ok(PathBuf::from(value).join(APP_IDENTIFIER));
        }
        return Err("APPDATA is not set".to_owned());
    }
    #[cfg(target_os = "linux")]
    {
        if let Some(value) = std::env::var_os("XDG_DATA_HOME") {
            return Ok(PathBuf::from(value).join(APP_IDENTIFIER));
        }
        if let Some(home) = std::env::var_os("HOME") {
            return Ok(PathBuf::from(home)
                .join(".local/share")
                .join(APP_IDENTIFIER));
        }
        return Err("neither XDG_DATA_HOME nor HOME is set".to_owned());
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        Err("controller management is unsupported on this operating system".to_owned())
    }
}

fn boxed_error(message: String) -> Box<dyn Error> {
    Box::new(std::io::Error::new(std::io::ErrorKind::Other, message))
}

#[cfg(windows)]
fn show_fatal_error(message: &str) {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};
    let text: Vec<u16> = std::ffi::OsStr::new(message)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let title: Vec<u16> = std::ffi::OsStr::new("Controwly")
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    unsafe {
        let _ = MessageBoxW(
            std::ptr::null_mut(),
            text.as_ptr(),
            title.as_ptr(),
            MB_OK | MB_ICONERROR,
        );
    }
}

#[cfg(not(windows))]
fn show_fatal_error(message: &str) {
    eprintln!("{message}");
    // A desktop dialog is best-effort; no application behavior depends on
    // zenity being installed, and the process still returns nonzero.
    if let Ok(mut child) = std::process::Command::new("zenity")
        .args(["--error", "--title", "Controwly", "--text", message])
        .spawn()
    {
        let _ = child.wait();
    }
}
