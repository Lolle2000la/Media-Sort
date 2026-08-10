# AGENTS.md

## Build & verify

```bash
cargo build --workspace
cargo test --workspace
cargo fmt --all --
cargo clippy --workspace --all-targets -- -D warnings

# Benchmarks (divan; baseline vs optimized thumbnail/preview variants)
cargo bench -p benchmarks                          # all benches
cargo bench -p benchmarks --bench image_thumbnails # single bench file

# Documentation Website (Astro / Node.js 24 LTS)
cd website
npm ci
npm run render-demos  # Renders all demo flows to website/public/demos/
npm run build
```

Clang is required (libmpv-sys needs `libclang`). `cmake` + `nasm` (x86/x86_64 only) are required to build the vendored libjpeg-turbo (static, via `turbojpeg-sys`). AVIF decode uses dav1d (`avif-native` image feature): on Linux/macOS install the system library (`libdav1d-dev` / `brew install dav1d`, found via pkg-config); if not found it falls back to a vendored meson+ninja source build (this is the path used on Windows CI). On `aarch64-pc-windows-msvc` the vendored build uses `clang-cl` as the C compiler (not `cl`), because dav1d's meson needs `gas-preprocessor.pl` for MSVC ARM64 assembly under `cl`, which isn't installed — `clang-cl`'s integrated assembler handles dav1d's `.S` files and the `gaspp` path is skipped. See `release.yml`'s `CC`/`CXX` override for that matrix entry. On `i686-pc-windows-msvc` (cross-compiled from an x86_64 host) `avif-native` is disabled entirely via a target-conditional `image` feature in `media-sort-backend`/`benchmarks` Cargo.toml, since dav1d-sys's vendored meson build has no cross-file support; AVIF files there decode via the bundled ffmpeg pipe fallback, and `native_image_extensions()` in `media-sort-core` lists `avif` unconditionally so the scanner still sees them.

## Pre-commit hooks

Hooks run `cargo fmt --all --` and `cargo clippy --fix --allow-dirty --allow-staged --all-targets -- -D warnings`. Formatter and clippy always scan the whole workspace, not just staged files. If clippy fails the commit is rejected.

## Workspace layout

Five app crates in `crates/` with strict dependency order, plus a benchmark crate:

```
media-sort-gui       (iced 0.14, winit, wgpu — the app binary)
  ├─ iced-automation     (generic iced app automation & video rendering)
  ├─ media-sort-backend  (filesystem, media decoding)
  │    └─ media-sort-core    (settings, i18n, undo/redo, no system deps)
  └─ iced-mpv             (iced subscription/widgets for mpv video playback)
       └─ mpv-utils          (libmpv wrapper, worker protocol, rotation utils — no iced)

benchmarks           (divan benches for thumbnail/preview pipelines, see below)

website/             (Astro, Starlight docs template, React components)
```

`media-sort-core` must never depend on `media-sort-backend`, `iced-automation`, `iced-mpv`, `mpv-utils` or `media-sort-gui`. `mpv-utils` must never depend on `iced-mpv` or any iced crate — it is the dependency-light entry point for non-GUI consumers (benchmarks, tests).

## Code style

**No `mod.rs` files.** Prefer `parent.rs` + `parent/` directory over `parent/mod.rs`. Example: `state.rs` + `state/audio.rs` instead of `state/mod.rs` + `state/audio.rs`. This avoids the ambiguity of two possible locations for the module root and keeps filenames meaningful in editor tabs.

## libmpv

The GUI binary links libmpv at build time and loads it at runtime. The system must have `libmpv-dev` (or equivalent) installed to compile. Without it `cargo build` fails on the `libmpv-sys` crate.

The libmpv wrapper itself lives in `mpv-utils` (no iced/wgpu dependency): `MpvContext` (software render context, frame capture, `query_supported_extensions()`), the `VideoCommand`/`VideoEvent` worker protocol, `rotate_rgba` and `detect_video_rotation`/`Rotation`, plus the Linux zero-copy path (`gl_context.rs`: offscreen EGL + GBM, DMA-BUF export). `iced-mpv` layers the iced subscription, `VideoState` state machine and widgets on top and re-exports the `mpv-utils` items (`iced_mpv::MpvContext`, `iced_mpv::rotate_rgba`, ...). `media-sort-backend` and `benchmarks` depend on `mpv-utils` directly and compile without the iced/wgpu stack.

At startup the app queries mpv for supported formats via `demuxer-lavf-list` and builds the media type registry dynamically. Video/audio support depends entirely on the installed mpv version.

## Configuration



## Update mechanism

Updates come from GitHub Releases via velopack. **Security ordering:** `updater.rs::download_and_apply_async` downloads the `.nupkg` and its `.sig` itself, checks the size against the feed, PGP-verifies, and only then calls velopack's `download_updates()` — which early-returns because the file already exists. On Windows velopack's download step extracts the package's `Squirrel.exe` over the live `Update.exe`; the reorder guarantees that extraction only ever happens for PGP-verified content (previously an unsigned package could replace `Update.exe` before verification failed and purged only the packages dir, leaving a persistent malicious updater). The signature is re-verified immediately before `apply_updates_and_restart`, and `pre_startup_verify_packages` purges any pending package that fails verification before velopack's startup auto-apply runs. All feed-supplied file names (full, base and delta) are validated against path traversal, and the feed-supplied `version` is validated (no `/`, `\`, whitespace, `.`/`..`) before it is interpolated into the download URL.

**Failure semantics:** whole-packages-dir purges happen ONLY on PGP verification failures (security-motivated). Transient download/size failures delete only this run's artifacts (per-run unique temps via `unique_temp_path`, `<stem>.<label>.<pid>.<counter>`) so a previously staged, verified package from an earlier run survives a network blip. The signature download is capped at 1 MiB (`MAX_SIGNATURE_SIZE`) like the package cap. The package download **streams** chunk-by-chunk into its staged temp file (size-capped incrementally; never buffered whole in memory, so a near-1 GiB package does not cost 1 GiB of RAM), and only the small signature is buffered (`collect_capped`). Unverified content is never placed at the final paths: the package and its `.sig` are staged to unique temps, PGP-verified **against the staged files**, and only then promoted onto the final `*.nupkg`/`*.nupkg.sig` paths by `promote_staged_package` (a crash between download and verification leaves nothing but ignored temps; a transient signature failure cannot clobber a verified package). If a promotion rename fails with a stale/locked-destination error kind (`PermissionDenied`, on unix also `AlreadyExists`), the destination is removed and the rename retried once — any other rename failure propagates the ORIGINAL error and removes nothing. If the signature promotion fails after the package promotion succeeded, `promote_staged_package` rolls the package rename back onto the temp so an error path ends with neither file promoted (a new `.nupkg` next to a stale `.sig` would be a mismatched pair that startup verification purges the whole dir for). The `.partial`/`.sig` temps deliberately lack the `.nupkg` extension so `pre_startup_verify_packages` never scans a leftover temp as a staged package. `UpdateFailed` is surfaced to the user via the status banner (`status-update-failed`), not just logged. The riskiest logic (size caps, version validation, temp naming, rename retry, promote) lives in pure helpers with unit tests, not inside the async download closure.

Runtime config lives at `$CONFIG_DIR/media-sort/config.toml` (TOML). `$CONFIG_DIR` resolves via the `dirs` crate: `$XDG_CONFIG_HOME` (Linux), `~/Library/Application Support` (macOS), `%APPDATA%` (Windows).

On first launch the app silently migrates settings from the legacy WPF JSON config. In debug builds (`cfg!(debug_assertions)`) the migration reads `debug_config.json` instead of `config.json`:
- `$CONFIG_DIR/Image Sort/config.json` or `debug_config.json` (old WPF app)

When the `UI_TEST` environment variable is set to a non-empty value, the config path is overridden to `ui_test_config.toml` in the current directory. This is handled in `media-sort-core/src/settings/store.rs`.

## Internationalization

Fluent `.ftl` files in `resources/locale/{en,de,ja}/`. Adding a string requires entries in all three locales. The language is auto-detected from the system locale at startup.

## Tests

Tests live in `crates/*/tests/` as integration test files (not `#[cfg(test)]` modules). Run a single crate's tests:

```bash
cargo test -p media-sort-backend
cargo test -p media-sort-core
```

Run a single test file:

```bash
cargo test -p media-sort-backend --test filesystem_tests
```

Test fixtures are in `crates/media-sort-backend/tests/fixtures/`.

## Dev vs release builds

The dev profile sets `opt-level = 3` for all dependencies (`[profile.dev.package."*"]`). This keeps the GUI responsive in debug builds (iced/wgpu are slow without optimizations). Release builds use `lto = "fat"` and `codegen-units = 1`.

## Git

Conventional commits (`feat:`, `fix:`, `chore:`). The `docs/` directory is the GitHub Pages site served at `imagesort.org` via Jekyll. It is intentionally kept even though the help pages are outdated for v3.0.

`.Image-Sort-master/` is a local-only reference copy of the legacy WPF codebase. It is gitignored and must never be committed.

**CI Routing:** GitHub Actions path filtering decouples builds. Commits touching `website/` trigger the Astro production build and push to the `gh-pages` branch. Changes to application crates skip web deployment and route to the multi-platform Rust test matrices.

## Architecture pattern (iced / The Elm Architecture)

The GUI follows iced's TEA pattern with a unidirectional data flow:

- **Model** — `AppState` (`crates/media-sort-gui/src/state.rs:14`) holds all UI state
- **Messages** — `Message` enum (`crates/media-sort-gui/src/message.rs:8`) with nested sub-enums: `FolderMessage`, `MediaMessage`, `SettingsMessage`, plus a direct `Video(iced_mpv::PlayerMessage)` variant (the player's own message type carries both events and user intents — there is no separate `VideoMessage` enum)
- **Update** — `app::update()` (`crates/media-sort-gui/src/app.rs:10`) is the pure reducer, ~950 lines across `update/`, returning `Task<Message>` for side effects. Undo/Redo (`MediaMessage::Undo`/`Redo`) feed `AppState::start_async_media_scan(select_idx)`, which kicks off the same `scan_media_files` background scan as `open_folder` rather than blocking the UI thread on a synchronous rescan; `poll_background_channels` drains and finally re-selects the entry at `pending_select_index`. Tests that need `entries` populated before asserting drain the async scan via the `drain_async_scan` helper in `update/tests.rs` (loops `poll_background_channels` until `scan_receiver` is `None`). There is no synchronous scan path in production or tests.
- **View** — `app::view()` delegates to `main_layout_view()` which composes 10 view sub-modules. `media_grid_view` is **virtualized**: only the cards within the current scroll viewport (±5 cards of buffer, computed via the shared `viewport_window` helper from `state.media_grid.scroll`, mirroring `subscriptions/thumbnail_tracker::update_viewport`) are constructed per frame, with leading/trailing `space()` of the same width padding the row so the scrollable's offset math is unchanged. `viewport_window` clamps both bounds to the filtered-list length so a stale scroll snapshot (folder switch to a smaller folder, or a search query shrinking the list, before the next `on_scroll` event) can never slice past the entry list — `open_folder` also resets `scroll.offset_x` to 0 for the same reason. Per-card `iced::widget::Id`s (and the equivalent per-folder-node IDs in `folder_tree_view`) were dropped — none were ever read elsewhere, so this also removes the per-frame `Box::leak` memory leak.
- **Subscription** — `app::subscription()` (`crates/media-sort-gui/src/app.rs:50`) merges 4 streams into `Message`:

| Stream | Source | Purpose |
|--------|--------|---------|
| Tick | `iced::time::every(16ms)` | Main loop tick; handles deferred exit + settings save |
| Keyboard | `subscriptions::keyboard` | Raw key events via `winit`, matched against configurable keybindings |
| Events | `iced::event::listen()` | Window resize/move/close, mouse drag (divider resize) |
| Video | `iced_mpv::VideoPlayer::subscription_with` | mpv worker thread events (frame ready, playback progress, etc.); omitted in headless demo export via `app::demo_subscription` |

Note: `app::update()` (`crates/media-sort-gui/src/app.rs:10`) delegates to the `update` module (`crates/media-sort-gui/src/update.rs`), which holds the `#[cfg(test)] mod tests` unit tests (`update/tests.rs`).

## Module overview

### media-sort-core (no system deps)

| Module | Purpose |
|--------|---------|
| `actions/` | `ReversibleAction` trait + `MoveAction`, `RenameAction`, `DeleteAction`, `CopyAction`. **Symlink policy:** all action constructors refuse symbolic-link sources (`ActionError::SourceIsSymlink`), checked via the shared `path_utils::is_symlink` helper BEFORE and AFTER `canonicalize()` — `canonicalize()` would resolve the link and silently act on its TARGET (drag & drop accepts arbitrary OS paths), and the post-canonicalize re-check closes the swap window. `CopyAction::execute()` re-checks the source immediately before `fs::copy` (which follows links). `MoveAction` refuses an existing destination (`TargetExists`), enforced at construction AND inside `execute()`/`rollback()` — `rename(2)` silently replaces, and Undo/Redo must not clobber a file that appeared at the old path since the move; destination checks use `symlink_metadata()` so dangling symlinks count as existing. Create-folder input is validated with `RenameAction::validate_stem` (rejects `/`, `\\`, `.`, `..`, OS-illegal chars). |
| `history.rs` | Undo/redo with `done`/`undone` stacks of `Box<dyn ReversibleAction>` |
| `l10n.rs` | Fluent-backed localization, auto-detects system locale, `tr()` for lookups |
| `media_type.rs` | `MediaType` enum (Image/Video/Audio), global `MediaRegistry` (OnceLock), extension lists |
| `models.rs` | `MediaEntry`, `FolderNode`, `PinnedFolder` data types |
| `path_utils.rs` | Cross-platform path comparison utilities, `is_symlink` (single source of truth for the symlink-refusal policy), `atomic_write` (unique-temp + fsync + rename — used by the settings store), `unique_temp_path` (shared by the settings store and the updater; pid + counter keeps two app instances from racing on a shared temp name) |
| `settings/` | `SettingsStore` + sub-modules: `general`, `keybindings`, `metadata_panel`, `pinned_folders`, `window_position` |
| `build.rs` | Auto-generates `locales_codegen.rs` from `resources/locale/` (see below) |

### media-sort-backend

| Module | Purpose |
|--------|---------|
| `filesystem/scanner.rs` | `walkdir`-based media file discovery |
| `filesystem/trash.rs` | Delete-to-trash (wrapping `platform/trash.rs`) |
| `filesystem/watcher.rs` | `notify` + `notify-debouncer-mini` filesystem change events |
| `media/format_pipeline.rs` | Per-format thumbnail/preview dispatch (private, shared by thumbnail.rs + image_decoder.rs) |
| `media/image_decoder.rs` | Full-resolution image loading via `image` crate; `load_preview()` (capped preview decode) |
| `media/audio_decoder.rs` | `AudioPlayer` using `rodio` output + `symphonia` decoding |
| `media/thumbnail.rs` | Thumbnail generation (dispatches into format_pipeline) |
| `metadata/image_meta.rs` | EXIF extraction via `kamadak-exif` |
| `metadata/audio_meta.rs` | Audio tags via `id3`/`metaflac`/`mp4ameta` |
| `metadata/video_meta.rs` | Video metadata extraction |
| `platform/trash.rs` | OS-specific trash implementation |

### mpv-utils (no iced dependency)

| Module | Purpose |
|--------|---------|
| `mpv_context.rs` | `MpvContext` (software render context, frame capture, `query_supported_extensions()`), `MpvError`; GL path (`new_gl`, `render_gl_frame`) for the zero-copy worker |
| `gl_context.rs` | Linux-only offscreen EGL + GBM (dlopen'd EGL, **linked** libgbm): `GlContext`, `GbmDevice`/`GbmBo`, DMA-BUF → EGLImage → GL texture/FBO binding |
| `gl_interop.rs` | mpv GL render-context creation, `render_gl_fbo`, `SharedGpuFrameHandle`/`DmaBufHandle` (RAII fd) |
| `worker.rs` | `VideoCommand`/`VideoEvent` worker protocol (raw frames + `Rotation` metadata), `rotate_rgba`, `PlayerConfig`, `start_video_worker(_with)`, zero-copy GPU ring (`GpuGl`) |
| `rotation.rs` | `Rotation` enum + `detect_video_rotation` (mp4 tkhd / EXIF / mp4ameta) |

### iced-mpv (iced subscription/widgets for mpv)

| Module | Purpose |
|--------|---------|
| `state.rs` | `VideoState` (observable state + command methods, `select()` load-or-reset, `reset()`, auto-`Deactivate` on drop), `PlayerMessage`/`PlayerHandle`, `VideoEvent` domain events (`LoadFailed`, `PlayExternally`, one-shot `FrameReady`) |
| `subscription.rs` | `video_player_subscription(_with)` — spawns the worker with a `PlayerConfig` (max frame size), delivers `PlayerHandle` + raw `WorkerEvent`s as iced messages |
| `player.rs` | `VideoPlayer` — self-contained state+subscription wrapper (`subscription_with_config` etc.); `view()` (with the `ui` feature) maps `VideoAction` into your message type |
| `widget/` | `controls.rs` (`media_controls_view`, transport-agnostic `MediaControlsState` snapshot — also drives audio), `player.rs` (`video_player_view`, hides controls until the worker connects), `shader.rs` (wgpu `VideoPipeline`/`VideoPrimitive`/`video_shader_view`, `wgpu` feature) |
| `action.rs` | `VideoAction` enum — user intent emitted by the widgets |
| `testing.rs` | `testing::ready()` — test-utils helpers for driving `VideoState` without a worker |

The raw tokio command channel is hidden behind the opaque `PlayerHandle`; production callers never name `tokio::sync::mpsc::Sender<VideoCommand>`, and tests use `iced_mpv::testing::ready()` instead (enabled via the `test-utils` feature in `[dev-dependencies]`). The crate's features: `ui` (default) adds the `lucide-icons` widgets, `wgpu` (default) adds the shader stack, `test-utils` adds the test helpers — `--no-default-features` builds the subscription/state core with no wgpu dependency at all. All user intents flow through `PlayerMessage::Action(VideoAction)`; `VideoState::update` applies them and returns `VideoEvent`s the app must handle.

### iced-automation (generic, no system deps)

| Module / Sub-crate | Purpose |
|--------|---------|
| `automation.rs` | Generic iced app simulation/automation engine, bounding box queries, custom HUD wrapper views, update interceptor helper |
| `headless.rs` | Headless emulator runner, raw video screenshot loops, ffmpeg pipeline encoder |
| `macros/` | Procedural macros (`#[message]` and `#[state(Message)]`) to generate boilerplate and traits for automation |

### media-sort-gui

| Module | Purpose |
|--------|---------|
| `app.rs` | `update()`, `view()`, `theme()`, `subscription()`, helper async tasks |
| `main.rs` | Entry point: init mpv registry, load settings, launch iced application |
| `message.rs` | `Message` and sub-enum definitions |
| `demo.rs` | Consolidates both interactive demo initialization and headless video export |
| `state.rs` | `AppState` struct, folder tree logic, media scanning, `detect_media_type()` |
| `view/` | 11 view files: `main_layout`, `folder_tree`, `folder_panel`, `media_grid`, `media_preview`, `metadata_panel`, `control_panel`, `search_bar`, `settings_dialog`, `credits_dialog`, `overlay` (modal overlay + transient `status_toast`) |

Transient user feedback (refused drops, update failures, create-folder errors) goes through `AppState.status_message` (`StatusMessage { text, expires_at }`): set via `state.set_status(...)`, rendered as a non-blocking `status_toast` stacked above modals by `view/overlay.rs`, and expired by `handle_tick` against the tick's own `Instant` (sleep-proof). `open_folder` clears it. New user-visible strings need entries in all three locale files.
| `widgets/` | Custom widgets: `video_canvas`, `video_player` (controls), `video_shader` (wgpu), `rename_modal`, `create_folder_modal`, `folder_icon` |
| `subscriptions/` | `keyboard.rs`, `video_player.rs`, `prefetch.rs` (thumbnail generation) |

### Demo video export (parallel rendering)

`media-sort-gui --export` renders all flows × locales in parallel via rayon. To stay race-free:

- **Locale** is passed explicitly: `DemoConfig.locale` → `DemoApp::configure_settings_for_demo()` hook → `settings.general.locale`. Never mutate `LANG`/`LC_ALL` env vars for this — they are process-global.
- **Fixture dirs** are unique per render (`demo_<pid>_<counter>` in temp), created in `init_demo`.
- **Automation clock**: `AutomationState.real_time` must be `false` for headless export so flows advance only on deterministic virtual ticks (1 per frame). Interactive demos keep `real_time = true`. If both clocks drive the automation, wall-clock load fast-forwards steps relative to rendered frames.
- **Virtual cursor**: `VIRTUAL_CURSOR` (mirrors `AutomationState.virtual_cursor` for the headless render loop's hover drawing) is `thread_local!`. Each rayon render stays on one worker thread; a process-global cursor makes hover states flicker as renders overwrite each other.
- Flow specs reference fixture files by absolute name (e.g. `$DEMO_ROOT/mock 5.png`) — when renumbering/renaming `resources/MockState/`, update `resources/demo_flows/*.json` accordingly.

### website (Astro + Starlight)

| Directory / File | Purpose |
| --- | --- |
| `astro.config.mjs` | Multi-language routing configuration, Starlight options, and React integration layer |
| `src/content/docs/` | Localized technical documentation manuals (`.md` and `.mdx` extensions) |
| `src/components/` | Custom interactive UI layers (e.g., React keyboard maps) embedded into manuals |

## Settings

`SettingsStore` (`crates/media-sort-core/src/settings/store.rs:54`) has 5 sub-structs:

- `GeneralSettings` — locale, dark mode, reopen folder, update checks, GIF animation, folder tree width, video zero-copy (`video_zero_copy`) and hardware decoding (`video_hardware_decoding`) playback toggles
- `KeyBindings` — all user-configurable shortcuts
- `MetadataPanelSettings` — expanded state and panel width
- `PinnedFoldersSettings` — list of pinned folder paths as strings
- `WindowPosition` — left, top, width, height

Saves are **atomic** (temp file + rename); a symlinked config path is resolved first so the rename lands on the real target and the user's symlink stays intact.

Settings are persisted via a **dirty-flag + tick-coalesced** scheme, not eagerly. Call sites that mutate a setting call `state.settings.mark_dirty()` instead of `save()`; the 16 ms `Tick` subscription in `app.rs` (see `handle_tick`) then calls `state.settings.save_if_dirty()` once per tick, so the cost of `toml::to_string_pretty` + `std::fs::write` is amortized across at most one disk write per ~16 ms regardless of how many settings changed in that window. Crash-resilience granularity is also one tick (~16 ms of pending mutations); **force-flush** sites (`state.settings.save()` directly, not via `save_if_dirty`) remain at the three exit paths — `Message::Quit`, the `should_exit` branch of `handle_tick`, and `Message::EventOccurred(Window::CloseRequested)` — so a pending dirty state is always flushed before the process tears down. The `UI_TEST` env var swaps the config path to `ui_test_config.toml` for testing.

To add a new persisted setting:
1. Add the field to the appropriate sub-struct in `crates/media-sort-core/src/settings/`
2. Add `#[serde(default)]` (or a concrete default) so old configs remain loadable
3. If the setting has a UI toggle, call `state.settings.mark_dirty()` after mutation (the next `Tick` flushes it; the three exit paths above force-flush)
4. If the setting is exposed in the GUI view, update the **Application Settings** manual ([settings.mdx](website/src/content/docs/en/config/settings.mdx)) in all supported locales
5. If the configuration schema changes (new key/default/section), update the **Configuration File** manual ([config-file.mdx](website/src/content/docs/en/advanced/config-file.mdx)) in all supported locales

## Video pipeline

The video playback path is complex and worth understanding before touching:

1. **Startup** — `main.rs` queries mpv via `MpvContext::query_supported_extensions()` (from `mpv-utils`, re-exported by iced-mpv) and initializes the global `MediaRegistry`
2. **Subscription** — `video_player_subscription()` (iced-mpv) spawns a tokio `VideoWorker` task (in `mpv-utils`) that owns the `MpvContext` and runs an mpv event loop. Raw worker events are re-exported as `WorkerEvent` (distinct from the crate's `VideoEvent` domain events)
3. **Communication** — the GUI sends `VideoCommand` (Load, Seek, SetVolume, TogglePause, Stop, Deactivate, ReleaseSlot) through the opaque `PlayerHandle`; worker responds with `WorkerEvent` (FrameReady, GpuFrameReady, GpuReady, PlaybackProgress, Muted, Volume, Paused). All user intents flow through `PlayerMessage::Action(VideoAction)` — the GUI's `Message::Video(PlayerMessage)` carries them straight into `VideoState::update`, which returns `VideoEvent` domain events (`LoadFailed`, `PlayExternally`, one-shot `FrameReady`) that the app must handle. The worker's render cap is configurable via `PlayerConfig` (`video_player_subscription_with` / `VideoPlayer::subscription_with_config*`; default 960×540, uncapped only when the worker confirmed the zero-copy GPU path via `WorkerEvent::GpuReady`).
4. **Rendering** — two paths, both rendered by the custom wgpu shader (`widgets/video_shader.rs` in iced-mpv, `VideoPipeline`/`VideoPrimitive`):
   - **Software**: frames arrive as `WorkerEvent::FrameReady { rgba: Arc<Vec<u8>>, width, height, rotation }`, uploaded to a wgpu texture in `prepare`.
   - **Zero-copy (Linux)**: the worker swaps its software context for an offscreen EGL context (`mpv-utils`'s `gl_context.rs`, GBM + EGL, dlopen for EGL, **linked** libgbm) and renders mpv frames into a ring of 3 DMA-BUF-backed FBOs (`GpuGl` in `worker.rs`, `internal_format = GL_RGBA8` so the dma-buf bytes are R,G,B,A). Each frame's exported fd is handed to the GUI as `WorkerEvent::GpuFrameReady { slot, handle, ... }`; `glFinish` runs before the handoff. `VideoState` stores the handle, and `VideoPrimitive::prepare` imports it via `gl_import::import_dmabuf_to_wgpu` (raw `vkAllocateMemory` + `ImportMemoryFdInfoKHR` + dedicated allocation + `vkBindImageMemory` — `vkMapMemory` on imported dma-bufs is unsupported on RADV, so verification is GPU-side). The slot is released with `VideoCommand::ReleaseSlot(slot)` when the next frame is committed (or on deactivate), so the worker only re-renders buffers the GUI is no longer sampling. Linux build-time requirement: `libgbm-dev` (the `libgbm-dev`/`mesa-common-dev` packages; the runtime `libgbm.so.1` ships with Mesa). The DMA-BUF must be `DRM_FORMAT_ABGR8888` (`fourcc_code('A','B','2','4')` = 0x34324241) and GBM usage `GBM_BO_USE_RENDERING` = 4.
5. **Lifecycle** — When the user navigates away from a video (`VideoState::select(None)`), closes the application (`CloseRequested`/`Quit`) or the state is dropped, `Deactivate` is sent to stop mpv playback. On `MpvContext::drop` or channel disconnect, `player.stop()` is executed to ensure `libmpv` demuxer/decoder threads release media handles and do not block application teardown.

The `FrameReady` handler in `VideoState` only commits frames whose `path` matches `selected_path` and whose `ready` flag is set — the O(1) "is this still the selected entry" check — so a late frame from a previously selected video can never repopulate the state. The first committed frame of each load cycle surfaces as the one-shot `VideoEvent::FrameReady` domain event (the GUI uses it to clear `media_errors`).

**Rotation detection** runs once per load (a file's rotation cannot change mid-playback; the old per-second recheck re-parsed the container every second), followed by a single one-shot, property-only re-probe ~2 s later (`rotation_recheck_deadline` in `worker.rs`) — mpv can report `video-params/rotate` after the first frame, and the property probe does no file I/O, so the hostile-file cost stays bounded. The hand-rolled MP4 box walk in `rotation.rs` uses checked arithmetic, rejects boxes extending past EOF and caps the walk at 10k boxes — crafted extended-size boxes wrap the u64 position arithmetic and previously caused an infinite loop in release builds.

The entire pipeline depends on `libmpv-sys` at build time and a working `libmpv` installation at runtime. Without it, video playback silently does nothing (the sender is `None`) — `video_player_view` hides the transport controls until the worker connects, so the dead-controls trap is not visible to users. The headless demo export runs `app::demo_subscription` (everything except the video worker) so parallel renders don't spawn one `MpvContext` per instance.

**Video thumbnails** do NOT use mpv by default anymore: `prefetch::generate_thumbnail()` first tries `media::ffmpeg_pipe::extract_frame()` (backend) — an ffmpeg subprocess piping the first frame as PNG (auto-rotated, no ffprobe needed) — which is ~3× faster than the mpv poll loop. The mpv worker pool remains as fallback when no ffmpeg binary is found (or when ffmpeg rejects the file); each fallback frame goes through `MpvContext::capture_frame(path, 128, 128, 1s)` (mpv-utils), which wraps the old hand-rolled poll loop (load → pause → poll `has_frame_ready` → render → `rotate_rgba`) — the `VIDEO_THUMBNAIL_WORKER` threads in `subscriptions/prefetch.rs` just call it. `ffmpeg_pipe::find_ffmpeg()` looks next to the running executable first (release bundles), then PATH, and **caches the result in a process-global `OnceLock<Option<PathBuf>>`** — the first call runs the PATH scan + `ffmpeg -version` verify spawn and all subsequent calls return the cached `PathBuf::clone()` with no scan, no spawn, and no syscall. The cache lives for the lifetime of the process and is never invalidated. Windows packages bundle the static `ffmpeg.exe` from the self-hosted mpv-winbuild fork ([Lolle2000la/mpv-winbuild](https://github.com/Lolle2000la/mpv-winbuild), a fork of zhongfly/mpv-winbuild), which publishes fresh `mpv-dev-*/ffmpeg-*` archives for **all three arches** (x86_64, aarch64, i686 — so the win-x86 package no longer relies on the frozen shinchiro build) on a monthly schedule. The fork auto-syncs its toolchain LLVM major to the Rust nightly's LLVM major (subrandr objects must link with lld of the same major), and versions its build caches by `nightly+LLVM` so a major bump rebuilds the toolchain automatically. **Vendoring happens inside `mpv-utils`'s build script** (`crates/mpv-utils/build.rs`, `vendor-windows` feature, enabled by the CI/release workflows): the tag pin is the `DEFAULT_MPV_WINBUILD_TAG` constant, overridable via the `MPV_WINBUILD_TAG` env var (a `cargo:rerun-if-env-changed` directive forces a rebuild when it changes). The GitHub release API is queried with `curl -fL` (HTTP errors abort the build) plus a `GH_TOKEN` bearer header when the env var is set — CI steps export `GH_TOKEN: ${{ secrets.GITHUB_TOKEN }}` because the runner never injects it otherwise. Downloads are only skipped when the binaries exist AND `target/mpv-winbuild-<arch>/tag.txt` matches the resolved tag, so a monthly tag bump re-downloads even through the 7-day cargo `target/` cache. Extraction is member-filtered (`7z e <arc> "*libmpv-2.dll" "*libmpv.dll.a" ... -y`, wildcards match at any depth; bsdtar is the fallback), the archives are deleted afterwards, and every step is **abort-loud**: a failed step prints `cargo:warning=` and `exit(1)`s instead of silently linking stale binaries. The DLL/`ffmpeg.exe` are staged into `target/<triple>/<profile>/` **and** `.../deps/` (Windows resolves DLLs from the test binary's own directory first), and the mingw-style import library `libmpv.dll.a` is renamed to `mpv.lib` (libmpv-sys links `-lmpv`; mingw import libraries are COFF, so MSVC accepts the rename). The `bump-mpv-winbuild` workflow opens a monthly PR that bumps `DEFAULT_MPV_WINBUILD_TAG` when the fork publishes a new release — scheduled workflows only fire from the repo's *default* branch, so the cron stays dormant until v3.0 is merged into master (workflow_dispatch works from any branch, e.g. against v3.0 to test it before the merge), and it targets `inputs.base_branch || github.ref_name` so the PR base follows whichever branch it runs from. macOS bundles `brew` ffmpeg into `Contents/MacOS/` via dylibbundler; Linux uses host ffmpeg (optional). The same `extract_frame()` also serves as the AVIF fallback in `format_pipeline.rs`.

Both `ffmpeg` and mpv invocations go through **bounded worker pools** in `subscriptions/prefetch.rs`: `FFMPEG_THUMBNAIL_WORKER` spawns `available_parallelism().clamp(2, 16)` worker threads (ffmpeg is a lightweight short-lived subprocess, so the cap is sized to fit a typical visible set in 1–2 batches); `VIDEO_THUMBNAIL_WORKER` (mpv) spawns `available_parallelism().clamp(2, 4)` (each `MpvContext` is heavyweight — holds GPU resources, libmpv handle, persistent state). Both pull from a shared `mpsc` receiver (Arc-Mutex guarded, mirroring the watch-pool pattern). The queues are **bounded** (`sync_channel`, 256 slots), queue sends use a 5 s deadline (`send_with_timeout` — `SyncSender::send_timeout` is unstable on the MSRV toolchain), and response waits use timeouts, so a scroll-storm cannot grow memory unboundedly and requesters un-hang when a pool's workers all failed to start (e.g. no libmpv). The caps prevent scroll-storm fan-out from spawning N simultaneous subprocesses (default tokio blocking-pool ceiling is 512); see `benches/perf_candidates.rs` Group G for the wall-time/resource tradeoff (8-way burst on this dev host: 121 ms unbounded at peak=8 procs vs 170 ms bounded-to-4 at peak=4 procs; 20-way burst: see `g_extract_concurrent_*_20` variants for the visible-set-scale case that motivated cap=16 over cap=4).

**Thumbnail failure semantics:** `prefetch::ThumbnailError` distinguishes `Decode` (real file problem — recorded in `media_errors`, persistent "failed" badge) from `Transient` (queue timeout, dead pool, ffmpeg/mpv request timeout — NOT recorded, so the card stays un-thumbnailed and the next viewport pass re-queues it; recording timeouts would permanently badge a slow-but-valid video and block retries for the session).

## Audio player

Audio playback uses `rodio` for output and `symphonia` (all codecs) for decoding, independent of mpv. The `AudioPlayer` is created in `AppState::new()` and is `None` if initialization fails (non-fatal). Commands: `PlayAudio`, `PauseAudio`, `StopAudio`. There is no seek or progress tracking for audio.

## GIF handling

GIF files are classified as `MediaType::Video`, not `MediaType::Image`. The `MediaType::Video::extensions()` list includes `"gif"`, and native image extensions do not include it. One setting controls behavior:

- `animate_gifs` — whether GIFs animate in both the preview and grid thumbnails

At the file system level the `image` crate can decode GIF natively. The mpv path is also available if the installed mpv supports GIF demuxing.

## Two `detect_media_type` functions — beware

There are **two different** media type detection functions:

1. `MediaRegistry::determine_type()` in `media-sort-core/src/media_type.rs:87` — strict priority order (native image → native audio → mpv-discovered), uses the global OnceLock registry. Returns `None` for unknown extensions.
2. `detect_media_type()` in `media-sort-gui/src/state.rs:566` — simple linear scan of hardcoded extension lists from `MediaType::extensions()`. Defaults to `MediaType::Image` for unknown extensions.

The GUI scanner uses (2), metadata loading uses (1). This can cause mismatches if mpv discovers additional extensions at startup (e.g., a custom mpv build with extra demuxers). If you add formats, update both.

## Caching

| Cache | Type | Capacity | Purpose |
|-------|------|----------|---------|
| `thumbnail_cache` | `LruCache<PathBuf, Handle>` | 200 | Grid thumbnails |
| `image_cache` | `LruCache<PathBuf, Handle>` | 20 | Preview images (capped at 1920×1440, see below) |
| `media_errors` | `MediaErrorTracker` | 10,000 | Tracks media files that failed to read or decode along with error details; prevents retry spam (capped so a folder of a million bad files cannot grow a ~200 MB map) |
| `MediaGridState.lower_names` | `Vec<String>` (parallel to `entries`) | unbounded | Precomputed lowercase `file_name`s consumed by `filtered_entries`; rebuilt via `rebuild_lower_names()` and MUST be called from every site that mutates `entries` (`clear`/`extend`/`retain` in `state.rs::open_folder`, `update.rs::poll_background_channels`, `update/media.rs`, `update/folder.rs`) |

When selection changes, next/previous images are preloaded into `image_cache`.

## Per-format decode pipeline (benchmark-driven)

`media-sort-backend` dispatches thumbnail (`thumbnail::generate_thumbnail`) and preview (`image_decoder::load_preview`) generation per extension through `media/format_pipeline.rs`. Strategies were chosen from `cargo bench -p benchmarks` measurements, not guesses:

| Extension group | Strategy | Why |
|---|---|---|
| jpg/jpeg | libturbojpeg scaled DCT decode (1/2, 1/4, 1/8) + fast_image_resize; EXIF orientation applied AFTER resize from a single in-memory read | ~2× faster than image-crate decode |
| bmp, tga, qoi, ff/farbfeld | image-crate decode + fast_image_resize | 1.7–3× faster than `img.thumbnail()` |
| png, gif, tiff, pnm, hdr, exr | `load_image` + `img.thumbnail()` (image crate's built-in) | FIR is *slower* here (full RGBA intermediate / float→RGBA8 cost) |
| avif | native dav1d decode (`avif-native` image feature), ffmpeg CLI pipe as fallback | `avif` feature alone is encode-only |
| audio extensions | `extract_audio_cover` + fast_image_resize | cover art |
| everything else | `load_image` + `img.thumbnail()` fallback | — |

Previews are **capped at 1920×1440** (`load_preview` in `update/tasks.rs`) — there is no zoom UI and iced scales the widget anyway; this cuts preview memory by ~97% vs full-res RGBA (e.g. 9.8MB vs 96MB for a 24MP JPEG) and is faster for JPEG (turbojpeg scaled decode).

**Decode budgets (security):** every full decode runs under `image_decode_limits()` (`MAX_DECODE_DIMENSION` = 16384px per-side dim caps + 256 MiB alloc cap, set via `ImageReader::limits`), and the turbojpeg path rejects SOF headers above 16384px before allocating the scaled buffer, with an explicit `max_alloc` guard at the allocation site itself. Crafted headers (GIF/JPEG/PNG claiming 65535²) can no longer drive GB-scale transient allocations in the parallel thumbnail pipeline. The two shared entry points `decode_path_with_limits` / `decode_bytes_with_limits` (in `image_decoder.rs`) own the reader+limits+decode boilerplate — new decode paths should route through them, not hand-roll a fourth copy. Trade-off: image-crate paths reject RGBA buffers > 256 MiB (images ≳ 8192²) and JPEGs with any side > 16384px are rejected outright — acceptable for a sort UI, but keep the boundary in mind when changing the caps.

`image_decoder::decode_image_dimensions` (used by `metadata::image_meta::extract_image_metadata` for the "Dimensions" field) reads **headers only** via `ImageReader::into_dimensions()` and swaps the dims when EXIF orientation is 5–8 — no full pixel decode. This is ~1900× faster than the previous `load_image().dimensions()` path while preserving the EXIF-orientation behavior the metadata tests assert.

If you change these code paths, re-run `cargo bench -p benchmarks` to confirm you didn't regress the measured winning strategy.

## build.rs — locale code generation

`crates/media-sort-core/build.rs` scans `resources/locale/` and generates `locales_codegen.rs` into `OUT_DIR`. It:
- Discovers available locales from subdirectory names
- Reads `# locale-name: <display name>` comments from each locale's `main.ftl`
- Generates `AVAILABLE_LOCALES`, `locale_display_name()`, and `load_ftl()` functions
- Emits `cargo:rerun-if-changed` for each locale file so rebuilds trigger on FTL changes

The generated code is never committed — it lives in the build output directory.

## Platform-specific code

| Feature | Windows | macOS | Linux |
|---------|---------|-------|-------|
| Open externally | `cmd /C start` | `open` | `xdg-open` |
| Trash | `platform/trash.rs` (windows impl) | `platform/trash.rs` (macos impl) | `platform/trash.rs` (freedesktop impl) |
| Windows integration | `integration_with_windows` setting, `#[cfg(target_os = "windows")]` | N/A | N/A |
| Config dir | `%APPDATA%` | `~/Library/Application Support` | `$XDG_CONFIG_HOME` |
| Update mechanism | `check_for_updates_on_startup` toggle in settings | (same) | (same) |

## Test coverage

Integration tests live in:
- `crates/media-sort-core/tests/core_tests.rs`
- `crates/media-sort-backend/tests/` — `filesystem_tests.rs`, `metadata_tests.rs`, `audio_tests.rs`
- `crates/media-sort-backend/tests/fixtures/` — test media files

The GUI crate has `#[cfg(test)]` unit tests in `app.rs` and `state.rs` but **no integration test files** under `crates/media-sort-gui/tests/`. GUI tests use instantiated `AppState` with `SettingsStore::default()`.

There is no test suite for `media-sort-gui`. To run the existing tests:
```bash
cargo test -p media-sort-core
cargo test -p media-sort-backend
cargo test -p media-sort-gui          # runs #[cfg(test)] modules only
```

## Benchmarks crate

`crates/benchmarks` (divan) measures the thumbnail/preview pipelines against fixtures in `resources/MockState/`. It replicates the production code paths as `baseline_*` variants (they copy the logic, they do not call the GUI) and compares them against optimized variants in `src/variants.rs`. Video variants use `mpv-utils` directly (no iced/wgpu in the build) and skip gracefully when libmpv is unavailable:

- `benches/image_thumbnails.rs` — 128px grid thumbnails. Baseline: `load_image` + `img.thumbnail()`. Variants: fast_image_resize, single-read EXIF, zune-jpeg, turbojpeg scaled decode (1/8 / 1/4 DCT scaling + fast_image_resize). Turbojpeg is ~2× faster.
- `benches/preview.rs` — full-image preview decode. Baseline: full-res RGBA. Variants: downscale to a 1920×1440 box (fast_image_resize, zune, turbojpeg scaled).
- `benches/video_thumbnails.rs` — mpv poll-loop baseline vs. polling tweaks vs. `ffmpeg` subprocess extraction (ffmpeg is ~2.4× faster than the mpv loop — now the production default with mpv fallback).
- `benches/format_thumbnails.rs` / `benches/format_previews.rs` — per-format coverage for every `image` crate `default-formats` format (png, jpeg, gif, bmp, ico, tiff, webp, qoi, tga, hdr, exr, pnm, farbfeld; avif). Fixtures are generated at runtime into a temp dir by `src/fixture_gen.rs` (never committed). Known decoder gaps: the image crate's AVIF *decode* requires the `avif-native` feature (dav1d; `avif` alone is encode-only — enabled workspace-wide), and its DDS decoder is DXT-only (uncompressed DDS undecodable).
- `benches/perf_candidates.rs` — incremental perf candidates (A ffmpeg cache, B `resize_rgba` thread_local Resizer reuse, C `decode_image_dimensions` header-only, D `detect_media_type` HashMap, E `filtered_entries` pre-lowercased cache, F `settings.save` dirty-flag coalescing, G bounded ffmpeg concurrency burst, H `is_animated_gif` cached field). Each "baseline" variant records the *prior* production cost so the bench keeps demonstrating the win after the production change lands; the corresponding `_optimized` variant reflects the post-change cost. New optimization candidates should be added under a fresh group letter here, not folded into the format-specific bench files. Correctness tests for each group run via `cargo test -p benchmarks`.

## Media entry model

`MediaEntry` (`crates/media-sort-core/src/models.rs:6`) is `{ path, media_type, file_name, animated }`:

- `media_type` is set by `state::detect_media_type(path, animate_gifs=user_setting)` at scan time. For GIFs this in turn calls `image_decoder::is_animated_gif` — a **header-only scan** that counts image descriptors (0x2C) without decoding frames, since `GifDecoder` allocates `logical_screen_w * logical_screen_h * 4` (no limits) on the first frame read (a 15-byte crafted GIF could force a ~16 GiB allocation on the UI thread) — to decide whether static GIFs (`is_animated_gif == Some(false)`) get reclassified as `Image` regardless of the user setting.
- `animated` caches the raw `is_animated_gif` result (`Some(_)` for GIFs, `None` for non-GIFs) so `prefetch::generate_thumbnail` doesn't have to re-run the header scan — that path takes `(path, media_type, animated)` from the caller and skips its old `detect_media_type(path, false)` re-discovery. Future callers wanting an "Animated: true/false" metadata panel field can read this directly instead of re-opening the file.
- The scan-time classification lives in two parallel rays: `state.rs::open_folder` kicks off the rayon-based `scan_media_files` scan with `pending_select_index = Some(0)`, and `update::poll_background_channels` (incremental scan drain) classifies each entry with `detect_media_type` + `is_animated_gif`. `update::handle_media_scan_completed` receives pre-built entries from rayon-based `Task::perform` callers and should preserve the `animated` field.

The benchmarks crate builds the same vendored static libjpeg-turbo as the backend (cmake + nasm needed) and uses the `ffmpeg`/`ffprobe` CLIs for those variants; ffmpeg-dependent and mpv-dependent tests skip gracefully when the tools are unavailable. Correctness tests for every variant run via `cargo test -p benchmarks`.

## Key dependencies beyond Rust std

| Crate | Used for |
|-------|----------|
| `lucide-icons` | Icon font bundled at compile time via `.font()` |
| `rfd` | Native file/folder picker dialogs |
| `lru` | LRU caches |
| `wgpu` + `winit` + `ash` + `raw-window-handle` | Vulkan interop for video frames |
| `symphonia` (all codecs) + `rodio` | Audio decoding and output |
| `kamadak-exif` + `id3` + `metaflac` + `mp4ameta` | Metadata extraction |
| `notify` + `notify-debouncer-mini` | Filesystem watcher |
| `walkdir` | Media file scanning |
| `trash` | Cross-platform delete-to-trash |
| `fluent` + `fluent-bundle` + `unic-langid` | i18n |

## Maintaining documentation

### AGENTS.md
When you add a new module, dependency, build step, architectural decision, or test infrastructure, update this file. When you change how an existing system works (e.g., the video pipeline, settings persistence, media type detection), update the relevant section.

### Help pages (`docs/`)
The `docs/` directory is the GitHub Pages site at `imagesort.org`. As of v3.0 the help pages are outdated and intentionally kept for historical reference. If you update the user-facing UI or workflow, update the corresponding help page:
- `docs/help.md` (English)
- `docs/help/help.de.md` (German)

### Documentation website (`website/`)
The `website/` directory is the new Astro/Starlight documentation site deployed to `gh-pages`. When adding or updating docs content, work in this directory. The dev server runs with `astro dev --background`.
To render all automated demo videos (headless iced simulation) to the public assets directory for deployment, run `npm run render-demos` from the `website/` directory. Output videos are saved in `website/public/demos/`.

**CRITICAL REQUIREMENT:**
- **GUI Setting Changes**: If you create, change, or remove settings from the user interface, you **must** update the Application Settings page (`website/src/content/docs/*/config/settings.mdx`) in all languages.
- **Schema Changes**: If any configuration schema/TOML keys are added, updated, or removed, you **must** update the Configuration File page (`website/src/content/docs/*/advanced/config-file.mdx`) in all languages.

### Locale files
When you add user-facing strings, add entries to all three locale files (`resources/locale/{en,de,ja}/main.ftl`). The build script detects changes automatically.**

## Operational Capabilities & Environment

### Environment-Aware Execution Protocol
1. **Tool Introspection:** At the start of a task, inspect your active environment schema to identify any available specialized tools (e.g., semantic search providers, codebase indexers, memory managers, or specific file-parsing utilities).
2. **Conditional Prioritization:** - **If matching tools exist:** You must prioritize them over generic terminal commands (`bash`, `sh`) or custom manual scripts to achieve the task efficiently.
   - **If no matching tools exist:** Fall back freely to standard shell commands, core utilities, or manual file discovery methods to complete the objective.
3. **No Redundant Execution:** Do not manually replicate a task via command-line tools (like raw `grep` or manual script compilation) if an explicit environment tool is already configured and exposed to your context to handle it.
