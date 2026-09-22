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

/// The `uv` binary to run the backend with. In a built app this is the copy
/// bundled by `tauri.conf.json`'s `bundle.resources` (fetched via
/// `scripts/fetch-uv.sh` before packaging) rather than whatever `uv` happens
/// to be on the user's PATH -- or nothing at all. `uv` also manages its own
/// Python installs, downloading one that matches the backend's
/// `requires-python` on first run if needed, so this is the one binary that
/// makes the whole app work without Python *or* uv pre-installed. Dev builds
/// still use PATH, since whoever's building from source already has uv.
fn uv_path(app: &tauri::AppHandle) -> PathBuf {
    if cfg!(debug_assertions) {
        PathBuf::from("uv")
    } else {
        app.path()
            .resource_dir()
            .expect("could not resolve resource dir")
            .join("bin")
            .join("uv")
    }
}

/// Small persisted app preferences, currently just the projects folder.
/// Lives in the (hidden) app-data dir -- it's the *pointer* to where
/// projects live that's fine to keep out of sight, not the projects
/// themselves. Separate from the backend's own data_dir (git clone
/// workspace, etc.), which stays under app-data unconditionally.
#[derive(serde::Serialize, serde::Deserialize, Default)]
struct Preferences {
    projects_dir: Option<PathBuf>,
}

fn preferences_path(app: &tauri::AppHandle) -> PathBuf {
    app.path()
        .app_data_dir()
        .expect("could not resolve app data dir")
        .join("preferences.json")
}

fn load_preferences(app: &tauri::AppHandle) -> Preferences {
    std::fs::read_to_string(preferences_path(app))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_preferences(app: &tauri::AppHandle, prefs: &Preferences) -> Result<(), String> {
    let path = preferences_path(app);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let json = serde_json::to_string_pretty(prefs).map_err(|e| e.to_string())?;
    std::fs::write(path, json).map_err(|e| e.to_string())
}

/// Where user projects live. Unlike the backend's own internal working data
/// (git clone cache etc, which stays under `app_data_dir`/data unconditionally),
/// this is user-visible and user-choosable via the Settings dialog (see the
/// get/set_projects_dir commands below).
///
/// Defaults under `app_data_dir` (~/Library/Application Support/...) rather
/// than ~/Documents -- Documents is one of the folders macOS gates behind a
/// permission prompt, and that grant is tied to the app binary's code
/// signature. This app is only ad-hoc signed (no stable Developer ID team
/// identity), so every rebuild produces a signature TCC has never seen
/// before and re-prompts, regardless of any grant from a previous build.
/// Real users installing once won't hit that repeatedly, but it makes every
/// rebuild during development re-ask -- avoiding ~/Documents by default
/// sidesteps it entirely. A user who wants projects under Documents (or
/// anywhere else) can still point there via Settings; that grant comes from
/// the folder picker (NSOpenPanel), which isn't tied to code-signing
/// identity and survives rebuilds fine.
fn resolve_projects_dir(app: &tauri::AppHandle) -> PathBuf {
    if let Some(dir) = load_preferences(app).projects_dir {
        return dir;
    }
    // Existing installs from before this default changed already have real
    // projects sitting under the old ~/Documents location with no saved
    // preference pointing at it (they were always just relying on this same
    // fallback) -- keep resolving there for them instead of silently
    // switching to a new, empty folder and making their projects vanish.
    // Only a genuinely fresh install (no preference, nothing under the old
    // default yet) gets the new location.
    if let Some(old_default) = app.path().document_dir().ok().map(|d| d.join("Dagster Designer")) {
        if old_default.is_dir() {
            return old_default;
        }
    }
    app.path()
        .app_data_dir()
        .expect("could not resolve app data dir")
        .join("Projects")
}

/// Kills whatever's already listening on `port`, if anything. Guards
/// against a stale backend surviving from a previous run -- a crash, or
/// (what actually happened during testing) the app bundle on disk getting
/// replaced by a fresh install while an old instance was still running.
/// That old backend's working directory reference goes stale the moment
/// the bundle it pointed into is replaced, but the process itself doesn't
/// die and keeps squatting on the port -- so our own backend fails to
/// bind, and every request silently hits the broken orphan instead,
/// producing confusing "No such file or directory" errors that have
/// nothing to do with whatever the user actually clicked.
fn kill_stale_backend_on_port(port: u16) {
    let Ok(output) = Command::new("lsof").args(["-ti", &format!(":{port}")]).output() else {
        return;
    };
    let pids = String::from_utf8_lossy(&output.stdout);
    let mut killed_any = false;
    for pid in pids.lines().filter(|l| !l.is_empty()) {
        eprintln!("Killing stale process on port {port}: pid {pid}");
        let _ = Command::new("kill").args(["-9", pid]).status();
        killed_any = true;
    }
    if killed_any {
        // Give the OS a moment to actually release the port before we try
        // to bind it ourselves.
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn spawn_backend(app: &tauri::AppHandle) -> Child {
    kill_stale_backend_on_port(BACKEND_PORT);
    let dir = backend_dir(app);
    let uv = uv_path(app);

    // The backend defaults to ./data and ./projects relative to its CWD,
    // which is `dir` above -- fine in a stable dev checkout, but in a
    // packaged app `dir` is inside Resources/, which `tauri build`
    // regenerates from scratch on every rebuild, silently deleting
    // everything there. data_dir (backend-internal working state) goes
    // under Tauri's app-data dir, which survives rebuilds, reinstalls, and
    // app updates; projects_dir is user-visible and user-configurable (see
    // resolve_projects_dir). pydantic-settings picks up DATA_DIR /
    // PROJECTS_DIR automatically (env vars map to Settings field names) --
    // no backend code change needed.
    let app_data_dir = app
        .path()
        .app_data_dir()
        .expect("could not resolve app data dir");
    std::fs::create_dir_all(&app_data_dir).expect("could not create app data dir");
    let projects_dir = resolve_projects_dir(app);
    std::fs::create_dir_all(&projects_dir).expect("could not create projects dir");

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

    Command::new(&uv)
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
        .env("PROJECTS_DIR", &projects_dir)
        .stdout(Stdio::from(stdout_log))
        .stderr(Stdio::from(stderr_log))
        .spawn()
        .unwrap_or_else(|e| {
            panic!(
                "failed to start the Dagster Designer backend via `{:?} run` in {:?}: {e}\n\
                 In a dev build, make sure `uv` is installed and on PATH (https://docs.astral.sh/uv/).",
                uv, dir
            )
        })
}

/// Blocks (briefly, off the main thread via setup being sync-but-early) until
/// the backend is accepting connections, so the window doesn't flash a
/// connection-refused error while `uv` is still resolving/starting uvicorn.
/// Generous timeout because on a brand new install, `uv` may need to
/// download a Python build plus dagster and friends from PyPI before
/// uvicorn even starts -- a plain PATH `uv` with an already-warm cache
/// would normally be ready in a couple of seconds.
fn wait_for_backend(port: u16) {
    let addr = format!("127.0.0.1:{port}");
    for _ in 0..600 {
        if TcpStream::connect(&addr).is_ok() {
            return;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    eprintln!("warning: backend did not become ready on {addr} within 5m; continuing anyway");
}

/// Kills the backend child process, if any. Shared by every quit path
/// (red-button close, Cmd+Q, Dock > Quit) so none of them can leave an
/// orphaned `uv run uvicorn` process behind.
///
/// `child.kill()` alone only signals the tracked `uv run uvicorn` process --
/// `uv run` execs a real child `uvicorn` subprocess rather than replacing
/// itself, so SIGKILLing just the `uv` PID leaves that grandchild orphaned
/// and still bound to the port (confirmed live: it survives every quit).
/// Falling back to the same lsof-by-port kill already used to clean up a
/// previous run's stale backend on startup catches it regardless of process
/// tree depth.
fn kill_backend(app: &AppHandle) {
    let state: State<BackendProcess> = app.state();
    let taken = state.0.lock().unwrap().take();
    if let Some(mut child) = taken {
        let _ = child.kill();
    }
    kill_stale_backend_on_port(BACKEND_PORT);
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

/// Current projects folder, for the Settings dialog to display.
#[tauri::command]
fn get_projects_dir(app: AppHandle) -> String {
    resolve_projects_dir(&app).to_string_lossy().into_owned()
}

/// Changes where new projects are created and restarts the backend pointed
/// at the new folder, so it takes effect immediately without a full app
/// restart. Existing projects are NOT moved -- they simply stop showing up
/// (the files are untouched; moving the folder yourself would work, except
/// each project's own .venv embeds absolute paths and wouldn't survive the
/// move without being recreated, so we don't attempt that automatically).
#[tauri::command]
fn set_projects_dir(app: AppHandle, path: String) -> Result<(), String> {
    let new_dir = PathBuf::from(path);
    std::fs::create_dir_all(&new_dir).map_err(|e| format!("could not create {new_dir:?}: {e}"))?;

    save_preferences(&app, &Preferences { projects_dir: Some(new_dir) })?;

    kill_backend(&app);
    let child = spawn_backend(&app);
    let state: State<BackendProcess> = app.state();
    *state.0.lock().unwrap() = Some(child);
    wait_for_backend(BACKEND_PORT);
    Ok(())
}

fn main() {
    tauri::Builder::default()
        // Must be the first plugin registered. If the app is already
        // running, this fires in that existing instance instead of
        // letting a second one fully launch -- prevents two backends
        // ever fighting over the same port in the first place (rather
        // than just cleaning up after the fact, like
        // kill_stale_backend_on_port does for the cases this can't
        // prevent, e.g. a previous crash).
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        .manage(BackendProcess(Mutex::new(None)))
        .setup(|app| {
            let handle = app.handle().clone();
            let child = spawn_backend(&handle);
            let state: State<BackendProcess> = app.state();
            *state.0.lock().unwrap() = Some(child);
            wait_for_backend(BACKEND_PORT);

            let menu = build_menu(&handle, None)?;
            app.set_menu(menu)?;

            // Frosted-glass background behind the window content. Only the
            // nav sidebar is actually translucent (see App.tsx / index.css)
            // -- the main content area stays opaque -- so this only shows
            // through there. `Sidebar` is macOS's own material for exactly
            // this pattern (Finder/Mail's sidebar): it renders light in
            // light mode and dark in dark mode, matching the nav rail's own
            // adaptive color classes below rather than fighting them.
            #[cfg(target_os = "macos")]
            {
                use window_vibrancy::{apply_vibrancy, NSVisualEffectMaterial};
                if let Some(window) = app.get_webview_window("main") {
                    if let Err(e) = apply_vibrancy(&window, NSVisualEffectMaterial::Sidebar, None, None) {
                        eprintln!("apply_vibrancy failed: {e}");
                    }
                }
            }

            Ok(())
        })
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![set_page_menu, get_projects_dir, set_projects_dir])
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
