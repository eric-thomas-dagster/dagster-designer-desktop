# Dagster Designer Desktop

Tauri v2 (Rust) shell that wraps [Dagster Designer](https://github.com/eric-thomas-dagster/dagster_designer) as a native macOS app: it spawns the FastAPI backend as a sidecar process, loads the built Vite frontend, and adds a native menu bar (File/Edit/View/Project/Actions/Window), native notifications, and a native folder picker.

## Layout assumption

This repo is meant to be checked out as the `src-tauri/` directory *inside* a `dagster_designer` checkout, as a sibling of `frontend/` and `backend/` — `tauri.conf.json` and the root-level `package.json`'s build scripts both resolve paths relative to that layout (`../frontend/dist`, `../backend/app`, etc.).

```
dagster_designer/
├── frontend/
├── backend/
├── package.json          # `npm run app:dev` / `app:build` (@tauri-apps/cli)
└── src-tauri/            # <- this repo
```

To set it up from scratch:

```sh
git clone https://github.com/eric-thomas-dagster/dagster_designer.git
cd dagster_designer
git clone https://github.com/eric-thomas-dagster/dagster-designer-desktop.git src-tauri
npm install
npm run app:dev
```

## Why a separate repo

`dagster_designer` is the actual product (web app usable on its own); this wrapper is optional desktop packaging for it. Keeping it separate means the wrapper's Rust/Tauri concerns (build targets, code signing, bundle config) don't clutter the app repo's history, and the app repo stays deployable as a plain web app without any of this.
