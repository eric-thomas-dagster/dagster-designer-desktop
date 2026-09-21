// Prevents an extra console window from popping up on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::Duration;

use tauri::menu::{MenuBuilder, MenuItemBuilder, PredefinedMenuItem, SubmenuBuilder};
use tauri::{AppHandle, Emitter, Manager, State};

/// One action the current page wants reachable from the menu bar (e.g.
/// Monitors' "Refresh" / "Generate with AI" / "New Monitor" header buttons).
/// `id` is whatever the page wants back in the "menu-action" event; it's
/// namespaced with the page title before reaching the menu so two pages
/// using the same short id (e.g. both calling theirs "refresh") can't
/// collide.
#[derive(serde::Deserialize)]
struct PageAction {
    id: String,
    label: String,
    accelerator: Option<String>,
}

const BACKEND_PORT: u16 = 8000;

/// Holds the child process for the FastAPI backend so we can kill it when
/// the app quits. Without this the `uv run uvicorn` process would keep
/// running as an orphan after the window closes.
struct BackendProcess(Mutex<Option<Child>>);

/// Where the backend's source lives. In dev, that's the sibling `backend/`
/// directory in the repo; in a built app, `tauri.conf.json`'s
/// `bundle.resources` copies it into the app bundle's Resources dir instead.
fn backend_dir(app: &tauri::AppHandle) -> PathBuf {
    if cfg!(debug_assertions) {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../backend")
    } else {
        app.path()
            .resource_dir()
            .expect("could not resolve resource dir")
            .join("backend")
    }
}

fn spawn_backend(app: &tauri::AppHandle) -> Child {
    let dir = backend_dir(app);

    // User projects MUST live outside the app bundle/build output. The
    // backend defaults to ./data and ./projects relative to its CWD, which
    // is `dir` above -- fine in a stable dev checkout, but in a packaged
    // app `dir` is inside Resources/, which `tauri build` regenerates from
    // scratch on every rebuild, silently deleting every project the user
    // ever created. Point it at Tauri's app-data dir instead (macOS:
    // ~/Library/Application Support/<bundle id>/), which survives rebuilds,
    // reinstalls, and app updates. pydantic-settings picks up DATA_DIR /
    // PROJECTS_DIR automatically (env vars map to Settings field names) --
    // no backend code change needed.
    let app_data_dir = app
        .path()
        .app_data_dir()
        .expect("could not resolve app data dir");
    std::fs::create_dir_all(&app_data_dir).expect("could not create app data dir");

    // stdout/stderr MUST go somewhere that never blocks. `Stdio::piped()`
    // gives the child an OS pipe with a small (~64KB on macOS) kernel
    // buffer; since nothing here ever reads from it, the backend's own
    // print()s (this codebase has plenty, including full CLI install
    // output) eventually fill that buffer and the next write() blocks --
    // freezing the entire single-process backend, silently hanging every
    // subsequent HTTP request with no error anywhere. A log file has no
    // such limit, so redirect to one instead.
    let log_path = app_data_dir.join("backend.log");
    let stdout_log = std::fs::File::create(&log_path)
        .unwrap_or_else(|e| panic!("could not create backend log at {log_path:?}: {e}"));
    let stderr_log = stdout_log
        .try_clone()
        .expect("could not clone backend log handle");

    Command::new("uv")
        .args([
            "run",
            "uvicorn",
            "app.main:app",
            "--host",
            "127.0.0.1",
            "--port",
            &BACKEND_PORT.to_string(),
        ])
        .current_dir(&dir)
        .env("DATA_DIR", app_data_dir.join("data"))
        .env("PROJECTS_DIR", app_data_dir.join("projects"))
        .stdout(Stdio::from(stdout_log))
        .stderr(Stdio::from(stderr_log))
        .spawn()
        .unwrap_or_else(|e| {
            panic!(
                "failed to start the Dagster Designer backend via `uv run` in {:?}: {e}\n\
                 Make sure `uv` is installed and on PATH (https://docs.astral.sh/uv/).",
                dir
            )
        })
}

/// Blocks (briefly, off the main thread via setup being sync-but-early) until
/// the backend is accepting connections, so the window doesn't flash a
/// connection-refused error while `uv` is still resolving/starting uvicorn.
fn wait_for_backend(port: u16) {
    let addr = format!("127.0.0.1:{port}");
    for _ in 0..120 {
        if TcpStream::connect(&addr).is_ok() {
            return;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    eprintln!("warning: backend did not become ready on {addr} within 60s; continuing anyway");
}

/// Kills the backend child process, if any. Shared by every quit path
/// (red-button close, Cmd+Q, Dock > Quit) so none of them can leave an
/// orphaned `uv run uvicorn` process behind.
fn kill_backend(app: &AppHandle) {
    let state: State<BackendProcess> = app.state();
    let taken = state.0.lock().unwrap().take();
    if let Some(mut child) = taken {
        let _ = child.kill();
    }
}

/// A standard macOS menu bar (app / Edit / Window). Without this, the app
/// menu bar is effectively empty, which is also why copy/paste/undo/select-all
/// don't work in text fields by default: macOS dispatches those as menu
/// commands (`copy:`, `paste:`, ...) down the responder chain, and WKWebView
/// only receives them when a matching Edit-menu item exists to send them.
fn build_menu(
    app: &AppHandle,
    page: Option<(&str, &[PageAction])>,
) -> tauri::Result<tauri::menu::Menu<tauri::Wry>> {
    let quit = MenuItemBuilder::with_id("quit", "Quit Dagster Designer")
        .accelerator("CmdOrCtrl+Q")
        .build(app)?;
    let preferences = MenuItemBuilder::with_id("app:preferences", "Preferences…")
        .accelerator("CmdOrCtrl+,")
        .build(app)?;

    let app_menu = SubmenuBuilder::new(app, "Dagster Designer")
        .item(&PredefinedMenuItem::about(app, None, None)?)
        .separator()
        .item(&preferences)
        .separator()
        .item(&PredefinedMenuItem::services(app, None)?)
        .separator()
        .item(&PredefinedMenuItem::hide(app, None)?)
        .item(&PredefinedMenuItem::hide_others(app, None)?)
        .item(&PredefinedMenuItem::show_all(app, None)?)
        .separator()
        .item(&quit)
        .build()?;

    let edit_menu = SubmenuBuilder::new(app, "Edit")
        .item(&PredefinedMenuItem::undo(app, None)?)
        .item(&PredefinedMenuItem::redo(app, None)?)
        .separator()
        .item(&PredefinedMenuItem::cut(app, None)?)
        .item(&PredefinedMenuItem::copy(app, None)?)
        .item(&PredefinedMenuItem::paste(app, None)?)
        .item(&PredefinedMenuItem::select_all(app, None)?)
        .build()?;

    // Mirrors the left-nav rail's tabs (App.tsx's `navItems`) so the main
    // sections are jumpable without reaching for the mouse -- Cmd+1..9 for
    // the first 9, same convention as browser tabs. "Code" and "Resources"
    // are the 10th/11th slots and intentionally get no accelerator. The
    // frontend (App.tsx) owns actually switching the tab and ignores
    // "view:code" for Dagster+ projects, where that tab isn't shown.
    let view_items: [(&str, &str, Option<&str>); 11] = [
        ("assets", "Assets", Some("CmdOrCtrl+1")),
        ("ingestions", "Ingestions", Some("CmdOrCtrl+2")),
        ("dbt", "dbt", Some("CmdOrCtrl+3")),
        ("monitors", "Monitors", Some("CmdOrCtrl+4")),
        ("alerts", "Alerts", Some("CmdOrCtrl+5")),
        ("runs", "Runs", Some("CmdOrCtrl+6")),
        ("pipelines", "Pipelines", Some("CmdOrCtrl+7")),
        ("primitives", "Automation", Some("CmdOrCtrl+8")),
        ("library", "Library", Some("CmdOrCtrl+9")),
        ("code", "Code", None),
        ("resources", "Resources", None),
    ];
    let mut view_menu_builder = SubmenuBuilder::new(app, "View");
    for (id, label, accel) in view_items {
        let mut item_builder = MenuItemBuilder::with_id(format!("view:{id}"), label);
        if let Some(accel) = accel {
            item_builder = item_builder.accelerator(accel);
        }
        view_menu_builder = view_menu_builder.item(&item_builder.build(app)?);
    }
    let view_menu = view_menu_builder.build()?;

    // Mirrors the "Project" dropdown in the app's own header (New/Import/
    // Open/Save) so those actions are reachable the way a Mac user expects
    // -- from the menu bar, with real accelerators -- not just from a
    // button inside the web content. Clicking these just emits a
    // "menu-action" event (see on_menu_event below); the frontend owns the
    // actual dialogs and behavior, same as when it's clicked in-page.
    let new_project = MenuItemBuilder::with_id("project:new", "New Project…")
        .accelerator("CmdOrCtrl+N")
        .build(app)?;
    let open_project = MenuItemBuilder::with_id("project:open", "Open Project…")
        .accelerator("CmdOrCtrl+O")
        .build(app)?;
    let import_project = MenuItemBuilder::with_id("project:import", "Import Project…").build(app)?;
    let import_dbt_cloud =
        MenuItemBuilder::with_id("project:import-dbt-cloud", "Import from dbt Cloud…").build(app)?;
    let open_dagster_plus =
        MenuItemBuilder::with_id("project:open-dagster-plus", "Open Dagster+ Project…").build(app)?;
    let save_project = MenuItemBuilder::with_id("project:save", "Save")
        .accelerator("CmdOrCtrl+S")
        .build(app)?;

    let project_menu = SubmenuBuilder::new(app, "Project")
        .item(&new_project)
        .item(&open_project)
        .separator()
        .item(&import_project)
        .item(&import_dbt_cloud)
        .item(&open_dagster_plus)
        .separator()
        .item(&save_project)
        .build()?;

    // Mirrors the in-page "Actions" dropdown. The one thing left out is its
    // "Launch Job" submenu of the project's specific jobs -- that list is
    // dynamic (fetched per-project) and rebuilding a native submenu on every
    // project switch is more machinery than this is worth right now, so
    // "Launch Job…" here just opens the same Launchpad in job mode and lets
    // the user pick from there.
    let materialize_all = MenuItemBuilder::with_id("actions:materialize-all", "Materialize All Assets").build(app)?;
    let open_launchpad = MenuItemBuilder::with_id("actions:open-launchpad", "Open Launchpad").build(app)?;
    let launch_job = MenuItemBuilder::with_id("actions:launch-job", "Launch Job…").build(app)?;
    let regenerate_lineage = MenuItemBuilder::with_id("actions:regenerate-lineage", "Regenerate Lineage").build(app)?;
    let discover_components = MenuItemBuilder::with_id("actions:discover-components", "Discover Components").build(app)?;
    let validate_project = MenuItemBuilder::with_id("actions:validate", "Validate Project").build(app)?;
    let generate_dockerfile = MenuItemBuilder::with_id("actions:dockerfile", "Generate Dockerfile").build(app)?;
    let generate_github_actions = MenuItemBuilder::with_id("actions:github-actions", "Generate GitHub Actions").build(app)?;
    let preview_code = MenuItemBuilder::with_id("actions:preview-code", "Preview Code").build(app)?;
    let export_project = MenuItemBuilder::with_id("actions:export", "Export Project…").build(app)?;

    let actions_menu = SubmenuBuilder::new(app, "Actions")
        .item(&materialize_all)
        .item(&open_launchpad)
        .item(&launch_job)
        .separator()
        .item(&regenerate_lineage)
        .item(&discover_components)
        .item(&validate_project)
        .separator()
        .item(&generate_dockerfile)
        .item(&generate_github_actions)
        .separator()
        .item(&preview_code)
        .item(&export_project)
        .build()?;

    let window_menu = SubmenuBuilder::new(app, "Window")
        .item(&PredefinedMenuItem::minimize(app, None)?)
        .item(&PredefinedMenuItem::maximize(app, None)?)
        .separator()
        .item(&PredefinedMenuItem::close_window(app, None)?)
        .build()?;

    let mut builder = MenuBuilder::new(app)
        .item(&app_menu)
        .item(&edit_menu)
        .item(&view_menu)
        .item(&project_menu)
        .item(&actions_menu);

    // The currently-active page's own header actions (Monitors' "Refresh" /
    // "Generate with AI" / "New Monitor", and similar per page), published
    // via the set_page_menu command below. Sits between the project-wide
    // Actions menu and Window so it reads as "this page's actions".
    if let Some((title, actions)) = page {
        if !actions.is_empty() {
            let mut submenu_builder = SubmenuBuilder::new(app, title);
            for action in actions {
                let id = format!("page:{title}:{}", action.id);
                let mut item_builder = MenuItemBuilder::with_id(id, &action.label);
                if let Some(accel) = &action.accelerator {
                    item_builder = item_builder.accelerator(accel);
                }
                submenu_builder = submenu_builder.item(&item_builder.build(app)?);
            }
            let page_menu = submenu_builder.build()?;
            builder = builder.item(&page_menu);
        }
    }

    builder.item(&window_menu).build()
}

#[tauri::command]
fn set_page_menu(app: AppHandle, title: Option<String>, actions: Vec<PageAction>) -> Result<(), String> {
    let page = title.as_deref().map(|t| (t, actions.as_slice()));
    let menu = build_menu(&app, page).map_err(|e| e.to_string())?;
    app.set_menu(menu).map_err(|e| e.to_string())?;
    Ok(())
}

fn main() {
    tauri::Builder::default()
        .manage(BackendProcess(Mutex::new(None)))
        .setup(|app| {
            let handle = app.handle().clone();
            let child = spawn_backend(&handle);
            let state: State<BackendProcess> = app.state();
            *state.0.lock().unwrap() = Some(child);
            wait_for_backend(BACKEND_PORT);

            let menu = build_menu(&handle, None)?;
            app.set_menu(menu)?;

            Ok(())
        })
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![set_page_menu])
        .on_menu_event(|app, event| {
            let id = event.id().as_ref();
            if id == "quit" {
                kill_backend(app);
                app.exit(0);
                return;
            }
            if id.starts_with("project:")
                || id.starts_with("actions:")
                || id.starts_with("app:")
                || id.starts_with("page:")
                || id.starts_with("view:")
            {
                // The frontend owns what happens next (opening its existing
                // dialogs, calling its existing save function, ...); we just
                // forward which item was picked.
                let _ = app.emit("menu-action", id);
            }
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { .. } = event {
                kill_backend(window.app_handle());
                window.app_handle().exit(0);
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running the Dagster Designer app");
}
