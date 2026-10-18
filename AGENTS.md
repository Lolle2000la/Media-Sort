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

Clang is required (libmpv-sys needs `libclang`). `cmake` + `nasm` (x86/x86_64 only) are required to build the vendored libjpeg-turbo (static, via `turbojpeg-sys`). On MSVC the vendored libjpeg-turbo defaults to the static CRT (`WITH_CRT_DLL=FALSE`, i.e. `/MTd` in debug builds), which collides with rustc's dynamic CRT and emits the benign `LNK4098 defaultlib 'LIBCMTD' conflicts` linker warning; `/NODEFAULTLIB:LIBCMTD`/`LIBCMT` in `.cargo/config.toml` (and mirrored in ci.yml's and release.yml's Windows `RUSTFLAGS`, since the env var REPLACES config rustflags) silences it — no CRT-managed memory crosses the boundary in this workspace's turbojpeg usage, and a future dependency that truly needs the static CRT would then fail at link time instead of warning. AVIF decode uses dav1d (`avif-native` image feature): on Linux/macOS install the system library (`libdav1d-dev` / `brew install dav1d`, found via pkg-config); if not found it falls back to a vendored meson+ninja source build (this is the path used on Windows CI). On `aarch64-pc-windows-msvc` the vendored build uses `clang-cl` as the C compiler (not `cl`), because dav1d's meson needs `gas-preprocessor.pl` for MSVC ARM64 assembly under `cl`, which isn't installed — `clang-cl`'s integrated assembler handles dav1d's `.S` files and the `gaspp` path is skipped. See `release.yml`'s `CC`/`CXX` override for that matrix entry. `avif-native` is enabled on every shipped target (`media-sort-backend`/`benchmarks` request it unconditionally): Windows uses the vendored meson+ninja dav1d path (`clang-cl` on aarch64, see `release.yml`), Linux/macOS prefer the system library (falling back to the vendored build if absent). `native_image_extensions()` in `media-sort-core` still lists `avif` unconditionally so the scanner sees AVIF even in a build without a native decoder (the backend can fall back to the ffmpeg pipe).

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

The libmpv wrapper itself lives in `mpv-utils` (no iced/wgpu dependency): `MpvContext` (software render context, frame capture, `query_supported_extensions()`), the `VideoCommand`/`VideoEvent` worker protocol, `rotate_rgba` and `detect_video_rotation`/`Rotation`. `iced-mpv` layers the iced subscription, `VideoState` state machine and widgets on top and re-exports the `mpv-utils` items (`iced_mpv::MpvContext`, `iced_mpv::rotate_rgba`, ...). `media-sort-backend` and `benchmarks` depend on `mpv-utils` directly and compile without the iced/wgpu stack.

At startup the app queries mpv for supported formats via `demuxer-lavf-list` and builds the media type registry dynamically. Video/audio support depends entirely on the installed mpv version.

## Configuration

The `general.theme` setting accepts the iced theme names shown in the settings dialog's pick list plus `"Auto"` (the default). `"Auto"` resolves through `app::theme()`: `AppState.system_theme` (`iced::theme::Mode`) holds the OS light/dark preference, resolved once at startup via `iced::system::theme()` and kept live by the `SystemThemeChanged` message from the `iced::system::theme_changes()` subscription (cross-platform: Windows registry/`UISettings`, macOS `AppleInterfaceStyle`, Linux desktop portal `color-scheme`). `Mode::None` (not yet resolved) falls back to Light. Explicit themes ignore the system mode entirely; legacy WPF configs still migrate their `DarkMode` bool to `"Dark"`/`"Light"`.



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

Dev builds use cargo's defaults — the root `Cargo.toml` deliberately has no `[profile.dev]` overrides. An aggressive `[profile.dev.package."*"] opt-level = 3` override existed historically to keep debug GUI testing responsive while the media pipeline was slow; it was removed once the decode pipeline improved. Only revisit it with measurements, since it slows every dependency compile. Release builds use `lto = "thin"` and `codegen-units = 1`.

## Git

Conventional commits (`feat:`, `fix:`, `chore:`). The `website/` directory is the GitHub Pages site served at `mediasort.app` by a GitHub Pages Actions deployment (the custom domain is declared in `website/public/CNAME`). The legacy Jekyll `docs/` tree was removed when the Rust rewrite landed — the Astro/Starlight site replaced it.

`.Image-Sort-master/` is a local-only reference copy of the legacy WPF codebase. It is gitignored (see the root `.gitignore`) and must never be committed.

**CI Routing:** `ci.yml` (`paths-ignore: website/**`, `README.md`) runs the Rust lint/test matrices directly on push to `master` and on pull requests. `website.yml` is the website's gate, and exists precisely because `ci.yml` ignores `website/**`: for any change under `website/` (or to the workflow itself) it runs `npm ci` strictly from the lockfile, `eslint`, `prettier --check` and `astro build`, on pull requests and on pushes to `master` — so a dependency bump without a matching lockfile update (the shape a `package.json`-only Renovate PR has) fails there instead of at deploy time. It deliberately does not render the demo videos: that needs the Rust GUI binary and a runner per flow, while the Astro build only references the videos by URL, and rendering is covered by `deploy-demos-website.yml`. The Astro site is deployed separately by `deploy-demos-website.yml` (triggered by `workflow_run` on the release workflow, or `workflow_dispatch`), which fans the work out across runners: `plan` lists the flow specs (one matrix entry per flow) and looks up the rendered-video cache, `build-app` compiles the demo export binary once and publishes it as an artifact, `render` runs one job per flow (that flow's three locales), `assemble` collects the videos, refuses to publish unless it found `flows × locales` of them, builds `website/` and uploads the Pages artifact, and `deploy` runs `actions/deploy-pages`. Rendering is the whole cost (~3.4 min per video: 1080p60, x264 crf=18, software Vulkan, debug build), so the videos are cached under a key over `crates/**`, `resources/**`, `Cargo.toml`, `Cargo.lock` and the workflow file — an unchanged run skips the compile and every render job, and `workflow_dispatch`'s `force-render` input bypasses the lookup; only this workflow writes that cache. The prebuilt binary finds `resources/MockState` through the compile-time `CARGO_MANIFEST_DIR`, which is why every job must use the default checkout path (no `path:` override, no container). Everything up to `deploy` is read-only; only the `deploy` job takes `pages: write` + `id-token: write` and claims the `github-pages` environment. Prerequisites outside the repo: Pages source set to **GitHub Actions**, and `master` listed in the `github-pages` environment's deployment branch policy (the workflow's ref for both triggers is the default branch).

**CI caching:** `ci.yml` caches Cargo artifacts + registry and bench fixtures via `actions/cache`, but only `push` runs on `master` may save caches — the `actions/cache/save` steps are gated on `github.event_name == 'push'` plus an exact-key miss, so pull-request and merge-queue runs are restore-only and every writable cache entry traces back to merged code. (GitHub also scopes PR-created caches to the merge ref; the gate makes the policy explicit rather than relying on that.) The lint job's `ubuntu-latest-cargo-*` archive holds only clippy's check-only dependency metadata; the test matrix uses `<os>-build-cargo-*` because metadata-only artifacts cannot serve a full build, and sharing one key left the ubuntu test job rebuilding every dependency on every run. `release.yml` must never restore a cache: published binaries are built from checked-out source only, so a poisoned cache cannot reach signed artifacts. The bench-fixture directory is excluded from the Cargo archive before saving so its ~330 MB per OS aren't counted twice against the repository's 10 GB cache budget.

## Releasing

Releases are tag-driven: pushing a `v*` tag runs `release.yml`, which builds the five shipped targets, packs and GPG-signs each Velopack `.nupkg`, uploads them to a **draft** release, then (in the `publish` job) prepends the tag message to a `git-cliff` changelog over the commits since the previous tag, merges duplicate releases for the tag, and flips the draft public (prerelease when the tag contains `-`, else `latest`). Stable releases also trigger `deploy-demos-website.yml`.

**Release notes carry an installer table.** A release carries a dozen-plus assets per platform (full/delta `.nupkg`s, their `.sig`s, update manifests, portable archives), which buries the handful of files a user wants. After the changelog is generated and the duplicate releases are merged, the `publish` job reads the draft's final asset list and slots a `## Downloads` table of direct installer links between the tag summary and the `## Changelog` heading (or leads the notes with it when no tag summary was set). The release is still a **draft** at that point, so the lookup goes through the paginated `/releases` list (the `/releases/tags/{tag}` endpoint never returns drafts — it 404s regardless of authentication) and the draft's `untagged-<hash>` asset URLs are rewritten to the canonical `releases/download/<tag>/…` URLs the assets serve from once published. The mapping lives in `scripts/release-downloads.sh`, which is pure: it reads `asset-name<TAB>browser_download_url` TSV on stdin (matching the names the website's `ReleaseDownloads` component shows) and prints nothing when no known installer is present — in which case the notes are left unchanged. **Add or rename a target in the `deploy` matrix and its installer/portable patterns must be updated here too.** The snap has no entry on purpose: it is published to the Store (see the snap section) rather than attached to the release, so there is no `.snap` asset to link.

**The version has several consumers, so the tag and every version file must agree.** Velopack takes the package version from the **tag name** (`v` stripped); the binary reports `[workspace.package].version` from `Cargo.toml` (the About dialog, and the updater's prerelease detection via `env!("CARGO_PKG_VERSION")`); and the OS reads two more files (below). The `verify-version` job in `release.yml` runs before any build and fails the whole run unless the tag matches all four. Treat `[workspace.package].version` as the single source of truth.

**Cut a release with `scripts/release.sh`** — do not hand-edit the version or hand-craft the tag:

```bash
scripts/release.sh 3.0.0-alpha.18 -F notes.md   # or -m "message"
scripts/release.sh 3.0.0-alpha.19 --dry-run     # validate only, change nothing
```

It runs preflight checks (clean tree, not behind upstream, tag unused locally and on `origin`), optionally verifies the HEAD commit's Actions runs are green (`--require-ci` to make that fatal; `gh`/`jq` used when present), bumps `[workspace.package].version`, refreshes `Cargo.lock` (`cargo update --workspace`, verified with `cargo metadata --locked`), commits `chore: bump version to X`, creates the **signed annotated tag** with the given message (your `commit.gpgsign`/`tag.gpgsign` key — hence the tag is created locally, never in CI), and pushes the branch and tag. Any failure before the commit rolls every version file back. Useful flags: `--no-push` (tag only), `--no-verify` (skip the CI check), `--dry-run`.

**Other version-bearing files.** The tag alone must determine every version in the shipped artifacts, so the same commit also syncs two OS-facing files:

- `packaging/macos/Info.plist` — `CFBundleShortVersionString` becomes the numeric `MAJOR.MINOR.PATCH` (prerelease stripped: Apple requires 1–3 period-separated integers, so `3.0.0-rc.1` → `3.0.0`), and `CFBundleVersion` (absent from the template) is added/updated as the **release ordinal**: 1-based over every existing `v*` tag, so the Nth release ships build N. Both are rewritten in place, so re-running is idempotent.
- `crates/media-sort-gui/media-sort-gui.rc` — a `VERSIONINFO` block generated between `// MS_VERSION_BEGIN` / `// MS_VERSION_END` markers. `FILEVERSION`/`PRODUCTVERSION` are `MAJOR,MINOR,PATCH,<release ordinal>` and the `FileVersion`/`ProductVersion` strings carry the full semver. It is compiled only on Windows (`build.rs`'s `embed_resource`), so it is inert on other targets.

`verify-version` re-derives all of these from the tag (`EXPECTED_BUILD` is the `v*` tag count, which includes the new tag) and fails the build on any mismatch, so a hand-cut tag cannot ship mismatched OS metadata.

## Snap packaging (Linux)

`snap/snapcraft.yaml` builds a **strictly confined** snap (`name: media-sort`, base `core24`, `platforms: amd64/arm64`). Its entire host access is the app's `plugs`: `home` auto-connects, the desktop/GPU/audio interfaces (`desktop`, `desktop-legacy`, `wayland`, `x11`, `opengl`, `audio-playback`) auto-connect, `gpu-2404` pulls in the `mesa-2404` graphics provider (below), and `removable-media` (`/media`, `/mnt`) plus `alsa` (raw-ALSA fallback) are connected on demand. Sorting files outside `$HOME` and removable media would need classic confinement, which the store's review policy does not grant for "the app wasn't designed with confinement in mind" file access, so the snap is strict and the AppImage stays the unrestricted option. Validated end-to-end on a desktop: launch/render, folder tree + thumbnails, mpv video preview, audio, sorting and the open-folder dialog all work under strict confinement.

- **Build:** by hand, `snapcraft pack --use-lxd` (any host with LXD) or `snapcraft pack --destructive-mode` (only when the host *is* the `core24` base, i.e. Ubuntu 24.04). CI (`.github/workflows/snap.yml`) builds on `ubuntu-latest` through Canonical's `snapcore/action-build` action, which installs and configures snapd + LXD + snapcraft and packs inside an LXD container pinned to the recipe's base — so the runner's Ubuntu release is irrelevant and the build sees only the recipe's `build-packages`, never the libraries the runner happens to ship. This is deliberate: with `--destructive-mode` the runner *has* to be `ubuntu-24.04`, so the runner label is load-bearing and must not be treated as a free dependency bump (a Renovate bump broke the job once). The build also clones the pinned `canonical/gpu-snap` commit for the `gpu-2404` helpers, so it needs GitHub access; bumping that pin means re-running the packaging check.
- **Part:** the `rust` plugin with `rust-path: [crates/media-sort-gui]`, so it runs `cargo install --locked --path crates/media-sort-gui` (installs the `media-sort-gui` binary into `bin/`), plus `rust-features: [media-sort-backend/pulse]`: a snap exposes the PulseAudio socket through `audio-playback`, and cpal's ALSA stream cannot deliver through the ALSA pulse plugin there (see the backend's `audio_decoder`), so the audio player writes to PulseAudio — the same arrangement as the Flatpak. The `velopack` feature is intentionally off — snapd owns updates, so the in-app self-updater is not built. `build-packages` carry libmpv/ALSA/PulseAudio/dav1d headers, `clang`/`libclang-dev` (libmpv-sys bindgen), and `cmake`/`nasm` (vendored libjpeg-turbo); `stage-packages` bundle `libmpv2`, `libasound2t64`, `libpulse0`, `libdav1d7`, `ffmpeg` and their transitive deps. The graphics libraries among those transitive deps are pruned again by the `gpu-2404` part, which is why the snap's only external runtime dependency is the `mesa-2404` provider. `override-build` symlinks the staged `ffmpeg` next to the binary (`bin/ffmpeg -> ../usr/bin/ffmpeg`) because `find_ffmpeg()` checks the binary's own directory before PATH, turning the lookup into a single stat.
- **Staged-library RPATH (`enable-patchelf`):** snapcraft's strict command-chain already puts `$SNAP/usr/lib/<triplet>` on `LD_LIBRARY_PATH`; `build-attributes: [enable-patchelf]` additionally writes an `$ORIGIN`-relative RPATH (`$ORIGIN/../usr/lib/<triplet>:...`) on the binary so the staged `libmpv.so.2` resolves even if that environment is not applied (DT_RPATH is inherited by the libraries' own dependencies).
- **Graphics userspace comes from the `gpu-2404` content provider, not from this snap.** Drivers must match the user's kernel and GPU, and the `core24` base ships none of its own (`/snap/core24/current/usr/lib/*/dri` and `/usr/share/vulkan` do not exist), so the recipe no longer ships GL/EGL/GLES/GBM/DRM/VA-API/VDPAU or any Vulkan ICDs. Instead: `plugs: gpu-2404` (`interface: content`, `target: $SNAP/gpu-2404`, `default-provider: mesa-2404`), plus a `gpu-2404` part that vendors upstream's [`canonical/gpu-snap`](https://github.com/canonical/gpu-snap) helpers — `bin/gpu-2404-wrapper` (the app's `command-chain` entry point: exports the provider's `LD_LIBRARY_PATH`/`LIBVA_DRIVERS_PATH`/`__EGL_VENDOR_LIBRARY_DIRS`/`VK_LAYER_PATH`/Vulkan-ICD search path, and also picks up the host's native Nvidia libs from `/var/lib/snapd/lib/gl*`) and `bin/gpu-2404-cleanup` (deletes everything `lists/mesa-2404.<arch>.list` says the provider supplies, so exactly one copy of each driver exists). Four decisions worth remembering: the part is **pinned to a `source-commit`** because the cleanup deletes files from the signed payload; the cleanup runs **without `--strict`**, so the list's trailing glob section catches this snap's staged Mesa whose file names track noble-updates rather than the provider's release (`libgallium-*`, `libdrm_*`, `dri/*`, `libvulkan_*`); there is deliberately **no `/usr/share/X11/XErrorDB` layout**, because the snap already ships that file from its own staged X11 libraries and a layout over a path the snap provides is a conflict; and the wrapper **hard-fails** when the interface is not connected, which is right because there are no drivers left in the snap to fall back to. `opengl` stays (auto-connected `/dev/dri` access). The packaging check installs and connects `mesa-2404` and asserts the cleanup pruned `libEGL_mesa`/`libva`/`dri`, but a runner has no GPU — **render it on a real desktop and an NVIDIA box before the first Store publish.**
- **Expected `snapcraft pack` lint output:** snapcraft's built-in linters run during `pack` and print warnings without failing the step, so `.github/workflows/snap.yml` reports a screenful of them. All of it is accounted for below, and the only entry that signals a real problem is a `gpu:` warning coming back:
  - `gpu: <lib>: GPU support library should be provided by a content interface` — **should not appear any more**: those libraries now come from the `gpu-2404` provider. If it comes back, `gpu-2404-cleanup` did not run — check the `gpu-2404` part's `override-prime` and its `after: [media-sort]`.
  - `library: <lib>: unused library '...'` — a false positive for everything reached through a **runtime-dlopen'd** library or a **subprocess**. The linter only walks static `DT_NEEDED` from `bin/media-sort-gui`, which lists just `libasound.so.2` and `libdav1d.so.7`: `libmpv.so.2` is opened at runtime and `ffmpeg` is exec'd, so their dependency closures look unused. The graphics entries (`libEGL_mesa`, `libGLX_mesa`, `libva*`, `libvdpau`) disappeared from this list along with the files; what remains (`libcaca.so.0`, `libcaca++`, `libzvbi.so.0`, `libtheora`, `libflite_*`, `libsphinxad`, `libjacknet`/`libjackserver`, `liblua5.2-c++`, `libhwy_contrib`/`libhwy_test`, `libicuio`/`libicutest`, `libfftw3_omp`/`libfftw3_threads`, `libcjson_utils`, `libzvbi-chains`) is transitive baggage that nothing in the snap references — size only. **Do not prune these**: `libcaca.so.0`, for instance, is a real `DT_NEEDED` of `libmpv.so.2` and `libavdevice.so.60`, so trimming it breaks loading.
  - `library: .../caca/libgl_plugin.so...: missing dependency 'libGLU.so.1' / 'libglut.so.3.12'` — libcaca's GL *output* plugin is only dlopen'd if something selects caca's GL driver, which nothing here does (the GUI renders through wgpu, mpv runs its own VO, ffmpeg never opens a caca device), so the absent dependencies are inert. `libx11_plugin.so` (caca's default driver) is the one that would be loaded, and it needs only libraries already staged.
- **Version:** `adopt-info: media-sort` + an `override-build` that reads `[workspace.package].version` from `Cargo.toml` and calls `craftctl set version`. The snap version is therefore the same single source of truth the binary reports and `verify-version` checks — no extra version-bearing file to sync in `scripts/release.sh`.
- **Desktop/icon:** `snap/gui/media-sort.desktop` and `snap/gui/media-sort.svg` (a copy of `packaging/linux/icon.svg`). snapd rewrites the `Exec`/`Icon` lines at install time; `Exec=media-sort` must match the app name exactly for that rewrite to apply.
- **Builds:** Canonical's [`snapcore/action-build`](https://github.com/snapcore/action-build) does the packing in both snap workflows, so the build runs inside an LXD container pinned to the recipe's base and the runner's Ubuntu release is irrelevant. `.github/workflows/snap.yml` is the recipe check (builds `amd64` when `snap/**`, a `Cargo.toml`/`Cargo.lock` or the workflow itself changes) and `.github/workflows/snap-release.yml` is the release path below. To build by hand: `snapcraft pack --use-lxd` on any host with LXD, or `snapcraft pack --destructive-mode` when the host *is* the base (Ubuntu 24.04). Launchpad was the alternative and was dropped: a `snapcraft remote-build --launchpad-accept-public-upload` copies the whole project directory before packaging — so run it from a `git worktree add --detach <dir> HEAD` of the tracked files, since a normal working tree here carries a multi-gigabyte `target/` — and it rejects non-repositories and shallow clones, so `git archive` or `--depth 1` will not do; its `auto_build` + `store_upload` recipes upload *and* release, but leave the same store-side promotion step behind. Publishing by hand is `snapcraft upload --release=latest/<candidate|stable> <file.snap>` with a logged-in snapcraft.
- **Publishing (`.github/workflows/snap-release.yml`):** builds every architecture in `platforms:` (`amd64` on `ubuntu-24.04`, `arm64` on `ubuntu-24.04-arm`, `fail-fast: false`), runs `scripts/check-snap-confinement.sh` on each built snap — the same check `snap.yml` makes, so a snap that cannot start is never published — and then, in a job on the `release` environment, uploads the revisions through `scripts/publish-snap.sh`. It is triggered by `release: published` (so it follows `release.yml`'s draft-flip) and by `workflow_dispatch`, which is how a release cut before this existed gets published, or a failed build retried. The channel follows the tag — a stable tag goes to `latest/stable`, a prerelease tag (one containing `-`) to `latest/candidate` — and a dispatch that does not name a channel is refused unless it points at such a tag, so `master` cannot reach stable by accident. Note that a dispatch runs the workflow as it exists at the ref you pick, so a release whose tag predates this file has to be published by dispatching on `master` with the channel named. A partial build publishes the architecture(s) that built and warns about the missing one, so one architecture cannot hold a release back; the job needs only `contents: read`, because the Store is the release record. Nothing is attached to the GitHub release.
- **Store credentials:** `SNAPCRAFT_STORE_CREDENTIALS` must exist on the `release` environment for the publish job (the environment that already holds the GPG secrets). Export a login scoped to this snap and to every channel it publishes to, and renew it before it lapses — a store login always carries an expiry (`snapcraft whoami` reports it; the CLI clamps long requests to a year):
  ```bash
  snapcraft export-login --snaps media-sort \
    --channels latest/stable,latest/candidate,latest/beta,latest/edge \
    /tmp/media-sort-store-login.txt
  gh secret set SNAPCRAFT_STORE_CREDENTIALS --env release < /tmp/media-sort-store-login.txt
  ```
  Without it `scripts/publish-snap.sh` fails with those instructions rather than publishing nothing quietly. A manual `snapcraft upload` needs only your own interactive login.
- **Install:** `sudo snap install media-sort`; offline `sudo snap install ./media-sort_*_amd64.snap --dangerous` (locally built snaps are unsigned). The Store install pulls in the `mesa-2404` provider automatically (`default-provider`); a `--dangerous` install of a local build does not, so exercising that build means `sudo snap install mesa-2404` and, if snapd did not connect it, `sudo snap connect media-sort:gpu-2404 mesa-2404` — without it the wrapper prints a message and exits rather than starting with no drivers. `removable-media` and `alsa` are **not** auto-connected, so files on `/media`/`/mnt` need `sudo snap connect media-sort:removable-media` and audio on a system with no sound server needs `sudo snap connect media-sort:alsa` (`audio-playback` covers every current Ubuntu desktop).
- **Store listing / website (deliberately dormant):** the site offers **no** snap right now. `https://snapcraft.io/media-sort` 404s until a revision is released, so a badge there would be a dead link; the installation pages (`getting-started/installation.mdx`, all locales) document the AppImage only, and nothing imports `website/src/components/SnapBadge.astro`. The badge itself (theme-aware, badge language following the page locale with `jp` for Japanese) and `snapStoreUrl` in `website/src/config.ts` are kept in the tree, unused and commented as such. **Restore when the listing is live**: import the badge in `DownloadGrid` (under the Linux button) and `ReleaseDownloads` (in the Linux group), and bring back the snap section on the installation pages — the diff to reverse is `fix(website): drop the snap offering until the listing is live`. The listing's own metadata (title/summary/description/license) was pushed with `snapcraft upload-metadata` and needs no site change.

## Flatpak (Flathub)

`flatpak/` holds the Flathub packaging: `app.mediasort.MediaSort.yml` (the manifest submitted to Flathub), `cargo-sources.json` (vendored crates generated from `Cargo.lock` with flatpak-cargo-generator), and a README with the full build/submit procedure. `portal-storage-feasibility.md` records why the document portal cannot replace the `home` grant. Upstream metadata lives in `packaging/linux/`: `app.mediasort.MediaSort.metainfo.xml` (AppStream), `app.mediasort.MediaSort.desktop`, and the shared `icon.svg`.

- **App id:** `app.mediasort.MediaSort`, reverse-DNS of the `mediasort.app` domain, so the app is eligible for the Flathub *Verified* badge (add Flathub's token to `website/public/.well-known/org.flathub.VerifiedApps.txt`). The manifest `app-id`, the metainfo `<id>`, the desktop file name, and the `<launchable>`/`Icon` values must all stay identical; the id is also the window app id at runtime.
- **Runtime:** `org.freedesktop.Platform`/`Sdk` `26.08` plus `org.freedesktop.Sdk.Extension.rust-stable`. No LLVM extension is needed — `libmpv-sys` 3.1 uses pre-generated bindings (no bindgen) and nothing else in the tree runs bindgen.
- **libmpv is built from source** (mpv v0.41.0, meson), together with the three build dependencies the runtime does not ship — `libass`, `libplacebo` and `zimg`. Everything the three link against comes from the runtime/SDK (ffmpeg libs, dav1d, libva, PulseAudio/PipeWire/ALSA, Wayland, freetype/fontconfig/harfbuzz/fribidi, vulkan/shaderc/lcms2) **and** the SDK's `ffmpeg` CLI at `/usr/bin/ffmpeg` (which `find_ffmpeg()` uses for thumbnails), so no codec stack is built here. Media Sort drives libmpv's **software render API**, so the mpv module disables its own GL/Vulkan output. **zimg is not optional:** the software renderer converts every frame with zimg when it is present and with swscale when it is not, and at 4K that is ~7 ms versus ~66 ms per frame — 30 fps versus ~6.6 fps with hardware decoding, so phone/camera 4K footage is unwatchable without it. The vendored libjpeg-turbo (`turbojpeg-sys`) builds with the SDK's cmake+nasm.
- **meson `libdir`:** the three meson modules (libplacebo, wayland-protocols, mpv) pass `--libdir=lib` in the *long* spelling, because meson's default inside the SDK is `lib64` — neither where the later modules' pkg-config search looks nor where the app links (`-L/app/lib`). Newer flatpak-builder passes `--libdir=lib` itself (it reads the SDK's `/etc/flatpak-builder/defaults.json`, whose `libdir` is a multiarch path), and meson rejects `-Dlibdir=x` next to `--libdir=y` ("Got argument libdir as both -Dlibdir and --libdir"), while the older flatpak-builder the CI job installs passes neither — so the long form is the one spelling that works with both. Symptoms of getting it wrong: `flatpak-builder` aborting in libplacebo (new builder) or libmpv landing in `/app/lib64` and the app failing to link `-lmpv` (old builder).
- **Hardware decoding:** VA-API is enabled. mpv gates it behind a display back-end, and both are missing a dependency in the runtime — Wayland wants `wayland-protocols`, X11 wants `libXpresent` — so both are built as small modules and `-Dx11=enabled -Dwayland=enabled -Dvaapi=enabled -Dvaapi-x11=enabled -Dvaapi-wayland=enabled` are passed, covering Wayland and X11 sessions alike. The `drm` back-end (which would avoid both) is impossible because mpv requires `sys/vt.h`, absent from the SDK.
- **No updater, no network:** the `velopack` feature is not built (Flatpak owns updates), so `finish-args` has no `--share=network`. The static permissions are `home` + removable-media mounts (the app is a file sorter and needs real paths, not document-portal ones), `--device=dri` (wgpu/Vulkan + VA-API), and `--socket=pulseaudio`. `--filesystem=home` trips the `finish-args-home-filesystem-access` linter error, which Flathub grants per-app on explanation (see the README).
- **Window app id:** `linux_desktop::application_id()` returns `FLATPAK_ID` verbatim when set, so the winit app id matches the exported `<id>.desktop` on both Wayland (icon/grouping) and X11 (`StartupWMClass`); the AppImage provisioning path is skipped under Flatpak.
- **Trash:** when `FLATPAK_ID` says we are sandboxed, `filesystem/trash.rs` deletes through `org.freedesktop.portal.Trash` (`filesystem/trash/portal.rs`) so the file reaches the *host* trash; the sandbox remaps `$XDG_DATA_HOME`, so the `trash` crate's home trash would be a private directory the desktop never shows. `restore` (undo) re-finds the entry by parsing the host trash's `.trashinfo` files, since the portal only deletes. Outside Flatpak the `trash` crate is used unchanged, and any portal failure falls back to it.
- **Audio files play through PulseAudio**, not through cpal's ALSA device: the manifest builds the `pulse` feature (`media-sort-backend`), which drives rodio's mixer straight into the PulseAudio socket. A sandbox exposes only that socket, and cpal's ALSA stream cannot deliver through the ALSA pulse plugin there — it opens, the client reaches the server, and then every read fails with `snd_pcm_avail_delay ... 'I/O error (5)'`, while `ffmpeg` and `mpv` use the same socket fine and no ALSA configuration (a `plug` wrapper, a pinned rate/format, blocking mode) changes that. mpv's audio — i.e. video sound — always worked, because it talks to PulseAudio directly. Without the feature (the normal build) nothing changes: the player keeps using cpal and falls back to it whenever no PulseAudio server answers.
- **Host hand-off (OpenURI):** *open externally* and *reveal in file manager* go through `org.freedesktop.portal.OpenURI` (`platform/desktop_portal.rs`) whenever `FLATPAK_ID` says we are sandboxed. The sandbox's session-bus policy lets an app talk only to its own name, `org.freedesktop.DBus` and `org.freedesktop.portal.*`, so the native `dbus-send` (`org.freedesktop.FileManager1`) route cannot work there at all. `OpenURI` takes file descriptors and has no select-a-file call, so revealing opens the containing folder rather than highlighting the entry; outside Flatpak both keep their native `open`/`explorer`/`xdg-open` implementations.
- **Releases:** Flathub builds from a released tag, so submission waits on a stable release that *contains* the packaging — `v3.0.0` predates `packaging/linux/app.mediasort.MediaSort.*`, so a follow-up (e.g. `3.0.1`) is needed. Bump the manifest `tag`/`commit`, add a `<release>` to the metainfo, and regenerate `cargo-sources.json` when `Cargo.lock` changes (the `Flatpak Cargo Sources` workflow fails if it is stale: it pushes the regenerated file on same-repo PRs and, on fork PRs where the token is read-only, puts the patch in the job summary and uploads the file as an artifact). Local checks: `flatpak run --command=flatpak-builder-lint org.flatpak.Builder manifest flatpak/app.mediasort.MediaSort.yml`, and with the SDK installed `flatpak-builder --user --install --force-clean build-dir flatpak/app.mediasort.MediaSort.yml` (point the app source at the checkout for uncommitted work).
- **CI:** `.github/workflows/flatpak-sources.yml` keeps `cargo-sources.json` in sync with `Cargo.lock` (path-filtered on the lock, the file and the workflow itself) and `.github/workflows/flatpak-build.yml` actually builds the Flatpak — with the manifest rewritten to `type: dir` so it builds the checkout rather than the release tag — whenever `Cargo.lock`, any `Cargo.toml` or `flatpak/**` changes. That catches a dependency edit that breaks the offline vendored build, or a `rust-version` bump past the SDK's pinned Rust, before it reaches Flathub. It is expensive, so it is not a required check. Neither workflow uses `pull_request_target` (see the note in `flatpak-sources.yml`).
- **Website:** `website/public/screenshots/*.png` are stills extracted from the rendered demo videos (the automation HUD painted out) and are referenced by the metainfo via `https://mediasort.app/…`, so the site must be deployed before the Flathub build mirrors them.

## Architecture pattern (iced / The Elm Architecture)

The GUI follows iced's TEA pattern with a unidirectional data flow:

- **Model** — `AppState` (`crates/media-sort-gui/src/state.rs:14`) holds all UI state
- **Messages** — `Message` enum (`crates/media-sort-gui/src/message.rs:8`) with nested sub-enums: `FolderMessage`, `MediaMessage`, `SettingsMessage`, plus a direct `Video(iced_mpv::PlayerMessage)` variant (the player's own message type carries both events and user intents — there is no separate `VideoMessage` enum)
- **Update** — `app::update()` (`crates/media-sort-gui/src/app.rs:10`) is the pure reducer, ~950 lines across `update/`, returning `Task<Message>` for side effects. Undo/Redo (`MediaMessage::Undo`/`Redo`) feed `AppState::start_async_media_scan(select_idx)`, which kicks off the same `scan_media_files` background scan as `open_folder` rather than blocking the UI thread on a synchronous rescan; `poll_background_channels` drains and finally re-selects the entry at `pending_select_index`. Tests that need `entries` populated before asserting drain the async scan via the `drain_async_scan` helper in `update/tests.rs` (loops `poll_background_channels` until `scan_receiver` is `None`). There is no synchronous scan path in production or tests.
- **View** — `app::view()` delegates to `main_layout_view()` which composes 10 view sub-modules. `media_grid_view` is **virtualized**: only the cards within the current scroll viewport (±5 cards of buffer, computed via the shared `viewport_window` helper from `state.media_grid.scroll`, mirroring `subscriptions/thumbnail_tracker::update_viewport`) are constructed per frame, with leading/trailing `space()` of the same width padding the row so the scrollable's offset math is unchanged. `viewport_window` clamps both bounds to the filtered-list length so a stale scroll snapshot (folder switch to a smaller folder, or a search query shrinking the list, before the next `on_scroll` event) can never slice past the entry list — `open_folder` also resets `scroll.offset_x` to 0 for the same reason. Per-card `iced::widget::Id`s (and the equivalent per-folder-node IDs in `folder_tree_view`) were dropped — none were ever read elsewhere, so this also removes the per-frame `Box::leak` memory leak.
- **Subscription** — `app::subscription()` (`crates/media-sort-gui/src/app.rs:50`) merges 5 streams into `Message`:

| Stream | Source | Purpose |
|--------|--------|---------|
| Tick | `iced::time::every(16ms)` | Main loop tick; handles deferred exit + settings save |
| Keyboard | `subscriptions::keyboard` | Raw key events via `winit`, matched against configurable keybindings |
| Events | `iced::event::listen()` | Window resize/move/close, mouse drag (divider resize) |
| Video | `iced_mpv::VideoPlayer::subscription_with` | mpv worker thread events (frame ready, playback progress, etc.); omitted in headless demo export via `app::demo_subscription` |
| Filesystem | `subscriptions::filesystem` | OS watcher events (adds/removes/renames) for the watched directory set, feeding `Message::FileSystemChanged` (see the Filesystem watcher section); also omitted from `demo_subscription` |

Note: `app::update()` (`crates/media-sort-gui/src/app.rs:10`) delegates to the `update` module (`crates/media-sort-gui/src/update.rs`), which holds the `#[cfg(test)] mod tests` unit tests (`update/tests.rs`).

## Module overview

### media-sort-core (no system deps)

| Module | Purpose |
|--------|---------|
| `actions/` | `ReversibleAction` trait + `MoveAction`, `RenameAction`, `DeleteAction`, `CopyAction`. **Symlink policy:** all action constructors refuse symbolic-link sources (`ActionError::SourceIsSymlink`), checked via the shared `path_utils::is_symlink` helper BEFORE and AFTER `canonicalize()` — `canonicalize()` would resolve the link and silently act on its TARGET (drag & drop accepts arbitrary OS paths), and the post-canonicalize re-check closes the swap window. `CopyAction::execute()` re-checks the source immediately before `fs::copy` (which follows links). Destination folders are always canonicalized at construction, so actions into a symlinked folder land in the real target directory. `MoveAction` refuses an existing destination (`TargetExists`), enforced at construction AND inside `execute()`/`rollback()` — `rename(2)` silently replaces, and Undo/Redo must not clobber a file that appeared at the old path since the move; destination checks use `symlink_metadata()` so dangling symlinks count as existing. Create-folder input is validated with `RenameAction::validate_stem` (rejects `/`, `\\`, `.`, `..`, OS-illegal chars). |
| `history.rs` | Undo/redo with `done`/`undone` stacks of `Box<dyn ReversibleAction>` |
| `l10n.rs` | Fluent-backed localization, auto-detects system locale, `tr()` for lookups |
| `media_type.rs` | `MediaType` enum (Image/Video/Audio), global `MediaRegistry` (OnceLock), extension lists |
| `models.rs` | `MediaEntry`, `FolderNode` (with `FolderKind` icon classification: `Default`/`Git`/`Symlink`/`Locked`, plus `symlink_target` for display), `PinnedFolder` data types |
| `path_utils.rs` | Cross-platform path comparison utilities, `is_symlink` (single source of truth for the symlink-refusal policy), `atomic_write` (unique-temp + fsync + rename — used by the settings store), `unique_temp_path` (shared by the settings store and the updater; pid + counter keeps two app instances from racing on a shared temp name) |
| `settings/` | `SettingsStore` + sub-modules: `advanced`, `general`, `keybindings`, `metadata_panel`, `pinned_folders`, `window_position` |
| `build.rs` | Auto-generates `locales_codegen.rs` from `resources/locale/` (see below) |

### media-sort-backend

| Module | Purpose |
|--------|---------|
| `filesystem/scanner.rs` | `walkdir`-based media file discovery |
| `filesystem/trash.rs` | Delete-to-trash: the `trash` crate, or — inside a Flatpak sandbox, where the platform remaps `$XDG_DATA_HOME` and the crate's home trash would be invisible to the desktop — `org.freedesktop.portal.Trash` (`filesystem/trash/portal.rs`), whose restore path re-finds the entry in the host trash by reading its `.trashinfo` metadata |
| `filesystem/watcher.rs` | `notify`-based OS watcher: `watch_directories()` watches any set of directories non-recursively, classifies events into `Added`/`Removed`/`Modified`/`Renamed` (drops `Access` noise) and debounces bursts through a private 100 ms debounce thread — `notify-debouncer-mini` was rejected because it reduces every event to `(path, Any)` and would destroy the kind information the GUI routing relies on. Each debounced burst is delivered as ONE channel message (a 500-file copy = 1 message), and consumers treat a batch as "something changed, re-scan", never as a complete diff. **Rename classification:** backends that pair both sides (inotify) emit one event with two paths → `Renamed(from, to)`; Windows delivers the two sides as separate single-path `Name(From)`/`Name(To)` events and macOS (kqueue — the workspace pins the `macos_kqueue` notify feature) emits `Name(Any)` with the old path only, so a single-path side is classified by existence — the side that still exists is `Added` (destination), the vanished side is `Removed` (source). The existence check runs at flush time (100 ms+ after the event; create-then-delete windows can misclassify — fine for a refresh heuristic). |
| `media/format_pipeline.rs` | Per-format thumbnail/preview dispatch (private, shared by thumbnail.rs + image_decoder.rs) |
| `media/image_decoder.rs` | Full-resolution image loading via `image` crate; `load_preview()` (capped preview decode) |
| `media/audio_decoder.rs` | `AudioPlayer` using `rodio` output + `symphonia` decoding. With the `pulse` feature, `audio_decoder/pulse.rs` writes to PulseAudio directly instead of a cpal device (the Flatpak enables it; see the Flatpak section) |
| `media/thumbnail.rs` | Thumbnail generation (dispatches into format_pipeline) |
| `metadata/image_meta.rs` | EXIF extraction via `kamadak-exif` |
| `metadata/audio_meta.rs` | Audio tags via `id3`/`metaflac`/`mp4ameta` |
| `metadata/video_meta.rs` | Video metadata extraction |
| `platform/desktop_portal.rs` | Linux sandbox integration: `in_flatpak()` (the single Flatpak check, re-exported by `filesystem/trash/portal.rs`) and the `org.freedesktop.portal.OpenURI` actions that hand a file to the host's default handler / file manager, which the sandbox's session-bus policy makes the only route |
| `platform/windows_shell.rs` | Windows Shell integration: registers/unregisters the "Open with Media Sort" Explorer context-menu entries in the registry |

### mpv-utils (no iced dependency)

| Module | Purpose |
|--------|---------|
| `mpv_context.rs` | `MpvContext` (software render context, frame capture, `query_supported_extensions()`), `MpvError`. Asks libmpv for its log messages and forwards them to `tracing` (`warn`/`error` by default, `info` — the chosen hwdec, the decoder, why an attempt failed — behind `RUST_LOG=debug`): libmpv hides them otherwise, and a Flatpak has no mpv CLI to run. Under verbose logging (`RUST_LOG=debug`/`trace`, or `MEDIA_SORT_MPV_TERMINAL`) mpv also writes to stderr at `msg-level=all=info`, because the event queue cannot be drained while a synchronous libmpv call blocks — which is precisely the "no frames" failure. `MEDIA_SORT_MPV_HWDEC` overrides the hardware decoder (default `auto-copy`), and the worker logs a `video frame report` every five seconds at debug level (`video_size`, `rendered`, `delivered`, `dropped`, `starved`, `render_avg_ms`) plus the first-frame latency — the numbers that separate "the worker cannot keep up" from "the consumer cannot" |
| `worker.rs` | `VideoCommand`/`VideoEvent` worker protocol (raw frames + `Rotation` metadata), `rotate_rgba`, `PlayerConfig`, `start_video_worker(_with)` |
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
| `main.rs` | Entry point: init mpv registry, load settings, launch iced application, and set up the tracing filter (`log_filter()`): `RUST_LOG` wins outright, so a bare `RUST_LOG=debug` works — while the default holds `iced_winit` at `warn`, because its info-level "Window attributes" line formats the window icon as `RgbaIcon { rgba: [262144 numbers] }`, one line per byte (~260k lines per start). Use `RUST_LOG=iced_winit=warn,debug` for debug output without it |
| `message.rs` | `Message` and sub-enum definitions |
| `demo.rs` | Consolidates both interactive demo initialization and headless video export |
| `state.rs` | `AppState` struct, media scanning, `detect_media_type()`; folder tree logic lives in `state/folder/tree.rs`, per-folder classification in `state/folder_inspection.rs` |
| `view/` | 11 view files: `main_layout`, `folder_tree`, `folder_panel`, `media_grid`, `media_preview`, `metadata_panel`, `control_panel`, `search_bar`, `settings_dialog`, `credits_dialog`, `overlay` (modal overlay + transient `status_toast`) |

Transient user feedback (refused drops, update failures, create-folder errors) goes through `AppState.status_message` (`StatusMessage { text, expires_at }`): set via `state.set_status(...)`, rendered as a non-blocking `status_toast` stacked above modals by `view/overlay.rs`, and expired by `handle_tick` against the tick's own `Instant` (sleep-proof). `open_folder` clears it. New user-visible strings need entries in all three locale files.
| `widgets/` | Custom widgets: `video_canvas`, `video_player` (controls), `video_shader` (wgpu), `rename_modal`, `create_folder_modal`, `folder_icon` |
| `subscriptions/` | `keyboard.rs`, `video_player.rs`, `prefetch.rs` (thumbnail generation), `filesystem.rs` (OS watcher subscription, see the Filesystem watcher section) |

### Demo video export

`media-sort-gui --export` renders all flows × locales **sequentially** (parallel rayon rendering was reverted — every render is CPU/wgpu-bound and parallel runs contend to the point of being slower overall). Rendering is sequential, but these rules still apply:

- **Locale** is passed explicitly: `DemoConfig.locale` → `DemoApp::configure_settings_for_demo()` hook → `settings.general.locale`. Never mutate `LANG`/`LC_ALL` env vars for this — they are process-global.
- **Fixture dirs** are unique per render (`demo_<pid>_<counter>` in temp), created in `init_demo`.
- **Automation clock**: `AutomationState.real_time` must be `false` for headless export so flows advance only on deterministic virtual ticks (1 per frame). Interactive demos keep `real_time = true`. If both clocks drive the automation, wall-clock load fast-forwards steps relative to rendered frames.
- **Virtual cursor**: `VIRTUAL_CURSOR` (mirrors `AutomationState.virtual_cursor` for the headless render loop's hover drawing) is `thread_local!`.
- Flow specs reference fixture files by absolute name (e.g. `$DEMO_ROOT/mock 5.png`) — when renumbering/renaming `resources/MockState/`, update `resources/demo_flows/*.json` accordingly.

### website (Astro + Starlight)

| Directory / File | Purpose |
| --- | --- |
| `astro.config.mjs` | Multi-language routing configuration, Starlight options, and React integration layer |
| `src/content/docs/` | Localized technical documentation manuals (`.md` and `.mdx` extensions) |
| `src/components/` | Custom interactive UI layers (e.g., React keyboard maps) embedded into manuals |

## Settings

`SettingsStore` (`crates/media-sort-core/src/settings/store.rs:54`) has 6 sub-structs:

- `GeneralSettings` — locale, dark mode, reopen folder, update checks, GIF animation, folder tree width, per-session pinned folders
- `KeyBindings` — all user-configurable shortcuts
- `MetadataPanelSettings` — expanded state and panel width
- `PinnedFoldersSettings` — list of pinned folder paths as strings
- `WindowPosition` — left, top, width, height
- `AdvancedSettings` — `disable_hardware_decoding` (video playback only, see the video pipeline section)

Saves are **atomic** (temp file + rename); a symlinked config path is resolved first so the rename lands on the real target and the user's symlink stays intact.

Settings are persisted via a **dirty-flag + tick-coalesced** scheme, not eagerly. Call sites that mutate a setting call `state.settings.mark_dirty()` instead of `save()`; the 16 ms `Tick` subscription in `app.rs` (see `handle_tick`) then calls `state.settings.save_if_dirty()` once per tick, so the cost of `toml::to_string_pretty` + `std::fs::write` is amortized across at most one disk write per ~16 ms regardless of how many settings changed in that window. Crash-resilience granularity is also one tick (~16 ms of pending mutations); **force-flush** sites (`state.settings.save()` directly, not via `save_if_dirty`) remain at the three exit paths — `Message::Quit`, the `should_exit` branch of `handle_tick`, and `Message::EventOccurred(Window::CloseRequested)` — so a pending dirty state is always flushed before the process tears down. The `UI_TEST` env var swaps the config path to `ui_test_config.toml` for testing.

**Multi-instance sync (field-granular saves + reload):** `save()` never writes the whole store anymore. The store keeps a `last_saved` snapshot (`toml::Value`, set on load/save/reload); on save it diffs the current serialization against the snapshot, and merges **only the changed fields** onto the freshly-read on-disk TOML (unknown keys from future versions survive — the reload baseline is re-serialized through the known structs so unknown keys can never be mistaken for removals). An unchanged store writes nothing (no mtime bump, no clobber). `reload_from_disk()` re-reads the file and adopts external changes field-by-field, with two rules: unflushed local changes (fields differing from `last_saved`) always win over disk, and **session-scoped** subtrees — `window_position`, `general.last_opened_folder`, `general.last_selected_media` (the `SESSION_SCOPED_PATHS` const), plus `pinned_folders` when `general.session_pinned_folders` is enabled (the scope decision is read from the *merged* state, so a concurrent toggle takes effect in the same reload) — are never adopted from disk and keep their local baseline. The merge is **best-effort**: the read-merge-rename in `save()` is not locked, so two instances saving within the same instant can still lose one update (last rename wins with a stale read); the 1 Hz reload poll heals that within a second, but do not rely on it for correctness-critical data. The GUI polls `reload_from_disk` once per second in `handle_tick` (gated by `AppState.settings_reload_at` + a `custom_path` check, before `save_if_dirty` so a reload never reverts an unflushed edit, and compiled out of demo builds so parallel headless renders stay deterministic); on a change, `update/settings.rs::apply_external_settings_changes` mirrors the derived UI state — locale + localized placeholders (`apply_locale_if_changed`, shared with `SettingsMessage::ChangeLanguage`), the `metadata.panel_expanded` mirror, and pinned folders (`AppState::sync_pinned_folders_from_settings`, which runs even when the store reloaded — with per-session pins enabled the reload simply never adopts external pins, so the sync is a no-op). Plain fields (theme, keybindings, widths, hardware decoding) are read live from `state.settings` by the view/subscription layers and need no mirroring.

To add a new persisted setting:
1. Add the field to the appropriate sub-struct in `crates/media-sort-core/src/settings/`
2. Add `#[serde(default)]` (or a concrete default) so old configs remain loadable
3. If the setting has a UI toggle, call `state.settings.mark_dirty()` after mutation (the next `Tick` flushes it; the three exit paths above force-flush)
4. If the setting is exposed in the GUI view, update the **Application Settings** manual ([settings.mdx](website/src/content/docs/en/config/settings.mdx)) in all supported locales
5. If the configuration schema changes (new key/default/section), update the **Configuration File** manual ([config-file.mdx](website/src/content/docs/en/advanced/config-file.mdx)) in all supported locales
6. If the setting must NOT be shared across running instances (per-instance session state), add its dotted path to `SESSION_SCOPED_PATHS` in `settings/store.rs` (or, for a user-selectable scope like `general.session_pinned_folders`, extend the dynamic scope decision in `reload_from_disk`)

## Video pipeline

The video playback path is complex and worth understanding before touching:

1. **Startup** — `main.rs` queries mpv via `MpvContext::query_supported_extensions()` (from `mpv-utils`, re-exported by iced-mpv) and initializes the global `MediaRegistry`
2. **Subscription** — `video_player_subscription()` (iced-mpv) spawns a tokio `VideoWorker` task (in `mpv-utils`) that owns the `MpvContext` and runs an mpv event loop. Raw worker events are re-exported as `WorkerEvent` (distinct from the crate's `VideoEvent` domain events)
3. **Communication** — the GUI sends `VideoCommand` (Load, Seek, SetVolume, TogglePause, Stop, Deactivate) through the opaque `PlayerHandle`; worker responds with `WorkerEvent` (FrameReady, PlaybackProgress, Muted, Volume, Paused). All user intents flow through `PlayerMessage::Action(VideoAction)` — the GUI's `Message::Video(PlayerMessage)` carries them straight into `VideoState::update`, which returns `VideoEvent` domain events (`LoadFailed`, `PlayExternally`, one-shot `FrameReady`) that the app must handle. The worker is configured via `PlayerConfig` (`video_player_subscription_with` / `VideoPlayer::subscription_with_config*`), which carries `hardware_decoding` plus a frame cap (`max_frame_width`/`max_frame_height`). `app::video_subscription` derives it from `AdvancedSettings::disable_hardware_decoding`: **hardware decoding is ON by default** (`hwdec=auto-copy`) and frames render at the video's **native resolution** (mpv defaults, no downscale); with hardware decoding disabled (`hwdec=no`) frames are capped at 960×540 to keep CPU decoding fast. The config is part of the subscription identity hash in `iced-mpv/subscription.rs`, so toggling the setting restarts the worker; `VideoState` re-loads the selected video on the new `Ready` handle.
4. **Rendering** — Frame RGBA data arrives as `WorkerEvent::FrameReady { rgba: Arc<Vec<u8>>, width, height, rotation: Rotation }`, stored in `VideoState`. The `video_player_view` widget (`widgets/video_player.rs` in the GUI) renders it via a custom wgpu shader (`widgets/video_shader.rs` in iced-mpv) for zero-copy Vulkan interop. This requires `ash` + `raw-window-handle` + `wgpu`.

   The worker keeps a **three-buffer pool** and hands each buffer downstream as an `Arc`, so a buffer is reusable only while the worker is its sole owner — the pool is the pipeline's backpressure. When the consumer holds all three (the GUI is a frame or two behind, which 4K frames make easy), the frame is skipped so mpv's `framedrop=vo` drops it. The obvious alternative — converting it into a throwaway buffer to "advance mpv's state machine" — pegs the CPU while every frame is discarded and starves the very consumer that has to release the buffers: measured at 0.15 fps on a 4K video whose normal rate is 30 fps, and it let the frame queue grow into the hundreds of MB.
5. **Lifecycle** — When the user navigates away from a video (`VideoState::select(None)`), closes the application (`CloseRequested`/`Quit`) or the state is dropped, `Deactivate` is sent to stop mpv playback. On `MpvContext::drop` or channel disconnect, `player.stop()` is executed to ensure `libmpv` demuxer/decoder threads release media handles and do not block application teardown.

The `FrameReady` handler in `VideoState` only commits frames whose `path` matches `selected_path` and whose `ready` flag is set — the O(1) "is this still the selected entry" check — so a late frame from a previously selected video can never repopulate the state. The first committed frame of each load cycle surfaces as the one-shot `VideoEvent::FrameReady` domain event (the GUI uses it to clear `media_errors`).

**Rotation detection** runs once per load (a file's rotation cannot change mid-playback; the old per-second recheck re-parsed the container every second), followed by a single one-shot, property-only re-probe ~2 s later (`rotation_recheck_deadline` in `worker.rs`) — mpv can report `video-params/rotate` after the first frame, and the property probe does no file I/O, so the hostile-file cost stays bounded. The hand-rolled MP4 box walk in `rotation.rs` uses checked arithmetic, rejects boxes extending past EOF and caps the walk at 10k boxes — crafted extended-size boxes wrap the u64 position arithmetic and previously caused an infinite loop in release builds.

The entire pipeline depends on `libmpv-sys` at build time and a working `libmpv` installation at runtime. Without it, video playback silently does nothing (the sender is `None`) — `video_player_view` hides the transport controls until the worker connects, so the dead-controls trap is not visible to users. The headless demo export runs `app::demo_subscription` (everything except the video worker) so parallel renders don't spawn one `MpvContext` per instance.

**Video thumbnails** do NOT use mpv by default anymore: `prefetch::generate_thumbnail()` first tries `media::ffmpeg_pipe::extract_frame()` (backend) — an ffmpeg subprocess piping the first frame as PNG (auto-rotated, no ffprobe needed) — which is ~3× faster than the mpv poll loop. **GIF files never take this path** (see the GIF handling section): a grid thumbnail is a static first-frame RGBA handle, so animated GIFs classified as `Video` are routed back through the in-process image pipeline. Every ffmpeg spawn in `ffmpeg_pipe.rs` goes through `ffmpeg_pipe::ffmpeg_command()`, which sets `CREATE_NO_WINDOW` (0x0800_0000) on Windows — the release GUI binary is built with `windows_subsystem = "windows"` and owns no console, so without the flag Windows allocates a NEW console window per spawned ffmpeg (scrubbing a video grid used to flash dozens of them). The mpv worker pool remains as fallback when no ffmpeg binary is found (or when ffmpeg rejects the file); each fallback frame goes through `MpvContext::capture_frame(path, 128, 128, 1s)` (mpv-utils), which wraps the old hand-rolled poll loop (load → pause → poll `has_frame_ready` → render → `rotate_rgba`) — the `VIDEO_THUMBNAIL_WORKER` threads in `subscriptions/prefetch.rs` just call it. `ffmpeg_pipe::find_ffmpeg()` looks next to the running executable first (release bundles), then PATH, and **caches the result in a process-global `OnceLock<Option<PathBuf>>`** — the first call runs the PATH scan + `ffmpeg -version` verify spawn and all subsequent calls return the cached `PathBuf::clone()` with no scan, no spawn, and no syscall. The cache lives for the lifetime of the process and is never invalidated. Windows packages bundle the static `ffmpeg.exe` and `libmpv-2.dll` from [zhongfly/mpv-winbuild](https://github.com/zhongfly/mpv-winbuild) (itself based on shinchiro/mpv-winbuild-cmake), which publishes fresh `mpv-dev-*/ffmpeg-*` archives for x86_64 and aarch64 automatically. It retains only the last 30 days of builds, so the release/CI workflows resolve the latest release tag at runtime instead of pinning one (a pinned tag would be pruned) — there is no `MPV_WINBUILD_TAG` pin and no bump workflow. 32-bit Windows (i686/win-x86) is no longer shipped, so no third-party 32-bit libmpv build is needed. The libmpv/ffmpeg download step is wrapped in `nick-fields/retry@v4`, which runs its `command` in a different working directory than `GITHUB_WORKSPACE` and cannot honor `working-directory` — so every path inside the command is `$GITHUB_WORKSPACE`-absolute (a relative `libs` would silently land outside the workspace and the linker's `/LIBPATH:libs` would fail with LNK1181). The retry action does not get `GH_TOKEN` in its environment (the runner never injects it — only `run` steps that set `env: GH_TOKEN: ${{ github.token }}` have it), so the step sets it explicitly, and since the retry action spawns the command *without* `set -e`, every retry-wrapped command starts with `set -euo pipefail`, uses `curl -fL`, and asserts the resolved asset names are non-empty — a failed `gh`/`curl` aborts loudly at the query site instead of silently downloading a 404 page and failing later at link time. The same rules apply to the retry-wrapped `Download git-cliff` step in the `publish` job (it `cd`s into `$GITHUB_WORKSPACE` before extracting). macOS bundles `brew` ffmpeg into `Contents/MacOS/` via dylibbundler; Linux uses host ffmpeg (optional). The same `extract_frame()` also serves as the AVIF fallback in `format_pipeline.rs`.

Both `ffmpeg` and mpv invocations go through **bounded worker pools** in `subscriptions/prefetch.rs`: `FFMPEG_THUMBNAIL_WORKER` spawns `available_parallelism().clamp(2, 16)` worker threads (ffmpeg is a lightweight short-lived subprocess, so the cap is sized to fit a typical visible set in 1–2 batches); `VIDEO_THUMBNAIL_WORKER` (mpv) spawns `available_parallelism().clamp(2, 4)` (each `MpvContext` is heavyweight — holds GPU resources, libmpv handle, persistent state). Both pull from a shared `mpsc` receiver (Arc-Mutex guarded, mirroring the watch-pool pattern). The queues are **bounded** (`sync_channel`, 256 slots), queue sends use a 5 s deadline (`send_with_timeout` — `SyncSender::send_timeout` is unstable on the MSRV toolchain), and response waits use timeouts, so a scroll-storm cannot grow memory unboundedly and requesters un-hang when a pool's workers all failed to start (e.g. no libmpv). The caps prevent scroll-storm fan-out from spawning N simultaneous subprocesses (default tokio blocking-pool ceiling is 512); see `benches/perf_candidates.rs` Group G for the wall-time/resource tradeoff (8-way burst on this dev host: 121 ms unbounded at peak=8 procs vs 170 ms bounded-to-4 at peak=4 procs; 20-way burst: see `g_extract_concurrent_*_20` variants for the visible-set-scale case that motivated cap=16 over cap=4).

**Thumbnail failure semantics:** `prefetch::ThumbnailError` distinguishes `Decode` (real file problem — recorded in `media_errors`, persistent "failed" badge) from `Transient` (queue timeout, dead pool, ffmpeg/mpv request timeout — NOT recorded, so the card stays un-thumbnailed and the next viewport pass re-queues it; recording timeouts would permanently badge a slow-but-valid video and block retries for the session). A third, non-error outcome covers "there is no image to show": `prefetch::generate_thumbnail` returns `Ok(None)` (surfaced as `MediaMessage::ThumbnailCancelled`) when an audio file has no embedded cover art, and the backend's `thumbnail::generate_thumbnail`/`thumbnail_image` returns `Ok(None)` for the same case. "No cover art" is a normal, playable file, NOT a decode failure: it must never be recorded in `media_errors` (that would show the preview pane's "file not supported" box and block playback). It used to fall back to decoding the audio file itself as an image, which failed with "the file extension `.wav` was not recognized as an image format". Genuinely corrupt embedded cover art still returns `Decode`/`Err`.

## Audio player

Audio playback uses `rodio` for output and `symphonia` (all codecs) for decoding, independent of mpv. The `AudioPlayer` is created in `AppState::new()` and is `None` if initialization fails (non-fatal). Commands: `PlayAudio`, `PauseAudio`, `StopAudio`. There is no seek or progress tracking for audio.

With the `pulse` feature (`media/audio_decoder/pulse.rs`) the output is PulseAudio instead of a cpal device: the player still owns a rodio `Player` (so pause/seek/volume/position are unchanged) but drives `rodio::mixer::mixer()` itself and writes the samples to the PulseAudio socket, because cpal's ALSA stream cannot deliver through the ALSA pulse plugin inside a sandbox. The Flatpak enables it; a machine without a PulseAudio server falls back to the default device.

## GIF handling

GIF files are classified as `MediaType::Video`, not `MediaType::Image`. The `MediaType::Video::extensions()` list includes `"gif"`, and native image extensions do not include it. One setting controls behavior:

- `animate_gifs` — whether animated GIFs are classified as `Video` (animated playback in the preview pane via the mpv video pipeline) or reclassified as `Image` (static decode)

Grid thumbnails are static first-frame RGBA handles either way, so `prefetch.rs::generate_thumbnail` routes `.gif` files (case-insensitive, `is_gif_path`) through the **in-process image pipeline** even when they are classified as `Video` — never through the ffmpeg/mpv video thumbnail pools. A thumbnail never needs the subprocess: the first frame is decoded natively by the `image` crate, which also keeps GIF thumbnails working on machines with neither ffmpeg nor libmpv.

At the file system level the `image` crate can decode GIF natively. The mpv path is also available if the installed mpv supports GIF demuxing.

## Two `detect_media_type` functions — beware

There are **two different** media type detection functions:

1. `MediaRegistry::determine_type()` in `media-sort-core/src/media_type.rs:87` — strict priority order (native image → native audio → mpv-discovered), uses the global OnceLock registry. Returns `None` for unknown extensions.
2. `detect_media_type()` in `media-sort-gui/src/state.rs:440` — simple linear scan of hardcoded extension lists from `MediaType::extensions()`. Defaults to `MediaType::Image` for unknown extensions.

The GUI scanner uses (2), metadata loading uses (1). This can cause mismatches if mpv discovers additional extensions at startup (e.g., a custom mpv build with extra demuxers). If you add formats, update both.

## Folder tree

The folder tree (`state/folder/tree.rs::build_children` / `build_parent_chain` / `build_tree_nodes_data`, rendered by `view/folder_tree.rs`) classifies every node via `inspect_folder()` (`state/folder_inspection.rs`), which stores a `FolderKind` on the `FolderNode`:

- **`Git`** — the folder contains a `.git` entry (lowest priority)
- **`Symlink`** — the folder is a symlink; `symlink_target` (resolved via `canonicalize`, `read_link` fallback for dangling links) is shown next to the node name
- **`Locked`** — contents not listable (read_dir `PermissionDenied`) or not writable (unix: no `0o222` write bits on the target's metadata; Windows: readonly attribute)
- **`Default`** — plain folder

Priority at build time: `Locked` > `Symlink` > `Git` > `Default`. `widgets/folder_icon.rs::icon_for()` maps kinds to lucide glyphs (FolderLock/FolderSymlink/FolderGit2/Folder/FolderOpen); the view overrides with `FolderBookmark` for pinned roots (depth 0, index > 0) and `FolderRoot` for filesystem roots (`path.parent().is_none()` — `/` on unix, drive roots on Windows; also used for the root of the parent chain). Parent-nav (chain) nodes keep the arrow-up glyph except at the filesystem root.

**Symlinked folders are followed transparently:** `build_children` includes symlink-to-directory entries, `read_dir` on them lists the target's children, expanding/selecting works like any other node, and `open_folder` canonicalizes the path up front so the tree, scanner and watcher all operate on the real target. Move/copy destinations are canonicalized by the action constructors, so sorting into a selected symlinked folder lands in the final target. The symlink-refusal policy for *file sources* is unchanged (see the `actions/` row above). Note `is_current` uses `paths_equal` (canonicalize-based), so the symlink node through which the current folder was entered is highlighted.

**Expansion state:** `ToggleExpand` carries the clicked node's *flat index* (same disambiguation as selection) because the current root and chain nodes can share a path (e.g. pinned breadcrumbs containing the current folder); path-only matching would toggle the wrong node. `toggle_folder_expand` falls back to re-resolving the path's current index when the click index is stale. Chain nodes are built with only nested breadcrumb children; expanding them rebuilds the real child listing via the shared `rebuild_node_children`, and `rebuild_expanded_children` in `build_tree_nodes_data` re-populates nodes that `restore_expansion` re-marked as expanded (previously they rendered as expanded with no real children and, with no chevron, could never be expanded until the folder was reopened).

## Filesystem watcher

External changes (adds, renames, moves, removes made outside the app) flow from the OS into the media grid and the folder tree through a watcher pipeline:

1. **Watched set** — `AppState::watched_directories()` (`state.rs`) computes the directories whose direct children are displayed: the current folder (media grid), every expanded folder-tree node, plus the current folder's **parent** (so a rename/delete of the current folder itself stays visible on backends that deliver no self-events — Windows reports nothing for a deleted current folder unless the parent is watched). Paths are canonicalized (symlinked nodes watch their real target) and filtered to existing dirs. The set is a pure function of the tree — no cache, so it can never go stale. It is recomputed per `subscription()` call (including each 16 ms tick — a tree walk plus a `canonicalize` per dir, trivial at UI scale).
2. **Subscription** — `subscriptions/filesystem.rs` keys an iced subscription on that set (`Subscription::run_with` with the sorted dir list + a `watch_generation` counter as the hashable id): iced restarts the stream whenever the set changes (folder switch, expand/collapse), so the OS watches always mirror the screen. The generation is bumped when a watched directory was deleted and recreated at the same path — the path list is unchanged, but the OS watch on the old inode is dead, so the identity must change to force a restart. The stream calls `media_sort_backend::filesystem::watcher::watch_directories()` and forwards each debounced burst as ONE `Message::FileSystemChanged(Vec<FileSystemEvent>)` — a 500-file copy is one routing pass, not 500. `FileWatcherHandle` lives inside the stream closure — dropping it (subscription restart/replacement) stops the watcher and its debounce thread. Excluded from `demo_subscription` so parallel headless renders stay deterministic.
3. **Routing** — `update/filesystem.rs::handle_filesystem_events` decides per event kind:
   - `Added`/`Removed`/`Renamed` whose parent is the **current folder** → media refresh
   - `Added` dir / `Removed`/`Renamed` of a path in the **folder tree** (or with a parent in the watched set) → tree refresh; a `Renamed` triggers the tree rebuild only for directories (`to.is_dir()` or the source was a tree node) — a file rename changes nothing the tree displays
   - `Modified` on a **directory** that is the current folder (→ media + tree refresh) or another watched dir (→ tree refresh) — the safety net for backends that under-report children; `Modified` on a **file** (content-only) is deliberately ignored — structure is unchanged
   - the current folder itself removed → media + tree refresh plus a `watch_generation` bump (re-establishes the watch if an external tool recreated the folder at the same path); if it no longer exists, the view moves to the closest existing ancestor instead
   - the current folder itself renamed → `open_folder` on the new path (follow), but only where the backend pairs rename sides (inotify, with the parent watched); where it can't pair (Windows/macOS deliver the two sides separately), the view moves up to the closest existing ancestor like a plain deletion
   - externally renamed pinned folders get their `pinned_folders` entry (and persisted settings path) updated to the new path — **known limitation:** this only runs on the paired-rename branch (inotify); on Windows/macOS the pin stays stale
4. **Media refresh** — `AppState::start_media_refresh()` starts a *replace-mode* scan (`MediaGridState.scan_replace` + `scan_buffer`): the existing entries stay visible while the scan runs (no grid blanking), and on completion `apply_refresh_selection` (`update.rs`) swaps the entries and restores the selection **by path** — same file selected, just at its new sorted index; only when the selected file vanished does it fall back to `select_and_load_entry` on the clamped old index. If the user had no selection, none is made (an external change never silently loads entry 0). The replace path bypasses the `reopen_last_selected_media` logic (that's folder-open semantics). Because every batch triggers a full rescan (never a partial diff), the grid self-corrects even when the backend under-reports events.
5. **Coalescing** — both refresh pipelines coalesce latest-wins instead of restarting on every event: if a scan/rebuild is in flight, `pending_refresh`/`tree_refresh_pending` is set, and `poll_background_channels` starts exactly one follow-up pass when the in-flight one lands. An event storm (copying hundreds of files) therefore costs at most one extra scan/rebuild after the last event instead of one thread per batch. `open_folder`/`start_async_media_scan`/`build_folder_tree` reset the refresh state (a folder switch always starts fresh).

**Platform backend notes:** Linux uses inotify (pairs rename sides, per-child events — the most precise backend; the "follow renamed current folder" branch is effectively inotify-only). Windows uses ReadDirectoryChangesWatcher (rename sides arrive as separate `Name(From)`/`Name(To)` single-path events; self-rename/delete of the current folder needs the parent watch, which is always active). macOS uses **kqueue** (the workspace pins notify's `macos_kqueue` feature, not FSEvents): it detects new children by scanning the directory on a `NOTE_WRITE` and reports only the FIRST not-yet-known entry, never reports deletions of pre-existing files (a directory-level `Modified` is emitted instead — hence the dir-`Modified` routing), and emits `Name(Any)` with the old path for renames of watched entries. The kqueue under-reporting is why consumers must treat any batch as "something changed, re-scan" rather than a complete diff.

In-app actions (move/delete/rename/copy) also trigger watcher events, so a redundant refresh runs shortly after — harmless by design (idempotent scan, selection preserved by path), it just keeps the grid consistent with the filesystem's view.

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
| audio extensions | `extract_audio_cover` + fast_image_resize | cover art; `Ok(None)` (no thumbnail) when the file has no embedded cover — never a decode fallback to the audio file itself |
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
| Open externally | `cmd /C start` | `open` | `xdg-open` (the `OpenURI` portal under Flatpak) |
| Trash | `filesystem/trash.rs` (windows impl) | `filesystem/trash.rs` (macos impl) | `filesystem/trash.rs` (freedesktop impl; `org.freedesktop.portal.Trash` under Flatpak) |
| Windows integration | `integration_with_windows` setting, `#[cfg(target_os = "windows")]` | N/A | N/A |
| Config dir | `%APPDATA%` | `~/Library/Application Support` | `$XDG_CONFIG_HOME` |
| Update mechanism | `check_for_updates_on_startup` toggle in settings | (same) | (same) |
| Window icon | `window::Settings.icon` (`packaging/windows/icon.ico`) | app bundle icon (`packaging/macos/icon.icns`) | X11: `window::Settings.icon`; Wayland: desktop-entry lookup (see below) |

## Linux desktop integration / Wayland window icon

Wayland compositors never read the window icon from the client. They resolve a toplevel's icon, display name and launcher grouping through the desktop entry whose file name (minus `.desktop`) matches the toplevel's `app_id`, which winit sets from `window::Settings.platform_specific.application_id`. An AppImage that was never "integrated" by appimaged/AppImageLauncher/Gear Lever has no such entry in the XDG data directories, so the shell falls back to a generic icon even though the AppImage ships `MediaSort.desktop` and the icon files itself.

`crates/media-sort-gui/src/linux_desktop.rs` (`application_id()`, called once from `main.rs`) closes that gap at startup:

1. If a packaged or hand-written `MediaSort.desktop` (the canonical id) exists anywhere, it wins: its name is used as the app id and nothing is written.
2. Otherwise it scans `$XDG_DATA_HOME/applications` and `$XDG_DATA_DIRS/*/applications` for a `*.desktop` file whose `Exec`/`TryExec` launches the running `$APPIMAGE` (or, when there is no AppImage, the running executable) - any integration tool, any file name; quotes, `env` prefixes and `\s` escapes are parsed. Paths inside the running AppImage's own `$APPDIR` mount are ignored - the compositor cannot see the mount, so its bundled entries must not count as "already registered" (AppImageKit's default `AppRun` prepends `$APPDIR/usr/share` to `$XDG_DATA_DIRS`). If found, its file name is adopted as the app id and nothing is written.
3. Otherwise - when no `MediaSort.desktop` exists anywhere - it writes a hidden `NoDisplay=true` desktop entry plus the app icon into `$XDG_DATA_HOME`, under a name unique to this module (`applications/mediasort-wayland.desktop` and `icons/hicolor/scalable/apps/mediasort-wayland.svg`, the same scalable icon the AppImage ships) via `path_utils::atomic_write`.

The provisioned files are kept between runs - they are what gives the shell the icon, and removing them at exit would only churn icon/menu caches and break window grouping across a restart. Because their name is unique to this module they can never collide with or shadow a packaged, distro- or tool-managed `MediaSort.desktop`, and a foreign file found at one of those paths is left untouched. They are removed again only when a canonical `MediaSort.desktop` appears (or when one was written under the canonical name by an earlier revision), so the packaged entry's icon and running state take over. Writes are skipped when content is unchanged so icon themes/menu caches are not re-scanned needlessly. Nothing is provisioned on non-Wayland sessions - on X11 the icon is delivered by the window itself (`window::Settings.icon`), so only the app id is set there (to the canonical `MediaSort`, which matches the packaged/Gear Lever `StartupWMClass`). The hidden entry adds nothing to application menus - it exists only so shells can find the icon - and it also makes `cargo run` and the standalone binary show the icon on Wayland (without an AppImage, `Exec` points at the running executable). Paths containing `%`, `$`, backticks or control characters are skipped: those have no `Exec` encoding that GLib's key-file layer/loader, KDE's parser and `desktop-file-validate` all agree on. Everything else - including spaces, `"` and `\` - is escaped for both the key-file and the shell parsing layer (verified against `desktop-file-validate`, GLib's loader and KDE's `KConfig`/`KShell`).

The embedded icon is `packaging/linux/icon.svg` itself (the scalable SVG the AppImage also ships), so it cannot drift from the packaged icon; GNOME (librsvg) and KDE (QtSvg) render theme SVGs natively. (`xdg-toplevel-icon-v1` would remove the need for all of this, but iced 0.14 pins winit 0.30, which cannot speak it; revisit when iced ships a newer winit.)

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
- `benches/format_thumbnails.rs` / `benches/format_previews.rs` — per-format coverage for every `image` crate `default-formats` format (png, jpeg, gif, bmp, ico, tiff, webp, qoi, tga, hdr, exr, pnm, farbfeld; avif). Fixtures are generated at runtime into `target/media-sort-fixtures` by `src/fixture_gen.rs` (never committed) — a persistent cache validated by a `VERSION` marker (bump `FIXTURE_VERSION` when generation parameters change). Generation runs in parallel (rayon) with per-process temp + rename promotion so concurrent nextest test processes cannot corrupt a shared fixture; JPEG uses the turbojpeg encoder. CI restores the dir via an actions/cache step keyed on `fixture_gen.rs` + the source image. Known decoder gaps: the image crate's AVIF *decode* requires the `avif-native` feature (dav1d; `avif` alone is encode-only — enabled workspace-wide), and its DDS decoder is DXT-only (uncompressed DDS undecodable).
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
| `notify` | Filesystem watcher |
| `walkdir` | Media file scanning |
| `trash` | Cross-platform delete-to-trash |
| `fluent` + `fluent-bundle` + `unic-langid` | i18n |

## Maintaining documentation

### AGENTS.md
When you add a new module, dependency, build step, architectural decision, or test infrastructure, update this file. When you change how an existing system works (e.g., the video pipeline, settings persistence, media type detection), update the relevant section.

### Help pages (`docs/`)
Removed with the Rust rewrite. The old Jekyll tree documented the v1.x/v2.x WPF UI (MSI installers, Microsoft Store links, .NET 3.1 / WiX build prerequisites) and is superseded by `website/`.

### Documentation website (`website/`)
The `website/` directory is the GitHub Pages site at `mediasort.app`, deployed by `.github/workflows/deploy-demos-website.yml` through the GitHub Pages Actions deployment (Pages artifact + `actions/deploy-pages`); the `gh-pages` branch is no longer part of the publish path. The demo videos are rendered one flow per runner and cached on the sources that determine them, so an unchanged run skips rendering entirely. The custom domain is declared in `website/public/CNAME`. When adding or updating docs content, work in this directory. The dev server runs with `astro dev --background`.
To render all automated demo videos (headless iced simulation) to the public assets directory for deployment, run `npm run render-demos` from the `website/` directory. Output videos are saved in `website/public/demos/`.

**Repository identity is resolved from its ID, never hardcoded.** The project was renamed Image-Sort → Media-Sort, and a future rename or transfer would change `owner/name` again (GitHub permanently redirects a renamed repository, but does not resolve a not-yet-existing one). `website/src/config.ts` holds the stable numeric `repoId` and resolves the current `owner/name` from it at build time via `gh api /repositories/<id>`, falling back to the current name when `gh` is unavailable (a plain `astro dev` must still render). Components and `astro.config.mjs` use the resolved `repoFull`/`repoUrl`. Docs content is written against the canonical name `Lolle2000la/Media-Sort`; `website/src/vite-canonical-repo.mjs` — wired as a `vite` pre-transform in `astro.config.mjs` — rewrites it in `.md`/`.mdx` sources to the resolved name, so link targets, visible link text and `git clone` snippets (including the bare `cd <repo>` line) all track the real name, and it is an identity mapping now that the rename has landed. It is a source transform rather than a remark/rehype plugin because Astro 7 requires `@astrojs/markdown-remark` for those, which would switch the whole site onto the legacy markdown pipeline.

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
