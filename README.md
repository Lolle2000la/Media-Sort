# Media Sort

Sort your media files fast. Version 3.0 is a rewrite in Rust.

The [documentation site](https://mediasort.app/) has a short demo video of every workflow.

## What is Media Sort?

Media Sort is a desktop app for sorting your media collection. It handles images, video, and audio files. You drive it from the keyboard.

### Why use it

- **It's fast.** Built in Rust, rendered with wgpu.
- **Keyboard-first.** All actions are bindable. Move, delete, rename, browse: your hands stay on the keyboard.
- **Undo and redo.** Moved a file into the wrong folder? Press Q. You can undo it. Press E to redo.
- **Pinned folders.** Pin folders you often sort into, then quick-move files with Alt+1 through 9.
- **In-app previews.** View images, play videos with seek and volume control, listen to audio. No need to open another app.
- **Metadata panel.** EXIF for photos, ID3/FLAC/MP4 tags for audio. Shown in a side panel you can collapse.
- **Multi-language.** English, German, Japanese. Picks up your system locale.

## Supported formats

| Type | Formats |
|------|---------|
| Images | PNG, JPEG, GIF, BMP, TIFF, ICO, WebP, JXL, HEIC, HEIF, AVIF |
| Audio | MP3, FLAC, OGG (Vorbis), WAV, AAC (M4A), WMA, Opus, AIFF |
| Video | MP4, MKV, WebM, AVI, MOV, WMV, FLV, M4V, and any other format your installed mpv can handle |

## How to use

> The central idea behind Media Sort is speed. The best way to use it is to keep your hands on the keyboard. But you can also use the mouse if you want. The app should help you either way.

### The folder tree

On the left is your folder tree. Navigate it with the WASD keys. W is up, S is down, A collapses a folder, D expands it. Hit Enter to open the selected folder, or O to pick some other folder.

### Pinned folders

Pin folders you use often with P (pin current) or F (pin selected). Unpin with U. After pinning, Alt+1 through 9 moves the current file to that pinned folder instantly. Reorder pins with Ctrl+W (up) and Ctrl+S (down).

This lets you sort files from one location into several destinations, even across different drives.

### Sorting

| Action | Shortcut |
|--------|----------|
| Move to selected folder | Up Arrow |
| Delete (move to trash) | Down Arrow |
| Previous file | Left Arrow |
| Next file | Right Arrow |
| Rename file | R |
| Undo | Q |
| Redo | E |
| Create new folder | C |
| Toggle search/filter | I |
| Toggle metadata panel | M |

All keybindings are customizable in the Settings dialog. Media keys (Play/Pause, Volume, Track skip) work for video and audio too.

## Privacy Policy

Read the [Privacy Policy](https://mediasort.app/about/privacy/).

No data is willingly collected. If you turn on "Check for updates on startup" in Settings, the app queries the GitHub API. That may collect data per GitHub's [privacy statement](https://help.github.com/en/github/site-policy/github-privacy-statement). Turn it off if you don't want that.

## Requirements

- Linux, macOS, or Windows
- Rust 1.98 or later (if building from source)
- libmpv (needed for video and audio playback)
- libdav1d (needed for AVIF image decoding)
- ffmpeg (bundled on Windows and macOS for fast video thumbnails; optional on Linux — falls back to mpv)

Install libmpv through your package manager:
- Ubuntu/Debian: `libmpv-dev`
- Fedora: `mpv-libs-devel`
- Arch: `mpv`
- macOS: `brew install mpv`
- Windows: download from [mpv.io](https://mpv.io/installation/)

Install libdav1d through your package manager:
- Ubuntu/Debian: `libdav1d7`
- Fedora: `dav1d-libs`
- Arch: `dav1d`
- macOS: bundled into the app automatically via `brew install dav1d` at build time
- Windows: bundled statically into the app

libjpeg-turbo (used for fast JPEG decoding) is vendored statically at build time — no installation needed.

## Building

```
git clone https://github.com/Lolle2000la/Media-Sort.git
cd Media-Sort
cargo build --release
cargo run --release
```

Build dependencies:
- Linux: `cmake` and `nasm` (x86_64 only) for the vendored libjpeg-turbo SIMD build, plus `libdav1d-dev` (strongly preferred — if missing, dav1d is compiled from source via meson, ninja, and git).
- macOS: `brew install dav1d` and `cmake` (preinstalled on most dev machines / available via brew). No nasm needed on Apple Silicon.
- Windows: `pip install meson ninja` and `choco install nasm` (x64/x86 only), in addition to the existing requirements.

Pre-built binaries for Linux, Windows, and macOS are provided with automatic updates via the GitHub Releases page.

## Installation

Pre-compiled standalone binaries with automatic updates are provided for Linux, Windows, and macOS via the [GitHub Releases](https://github.com/Lolle2000la/Media-Sort/releases) page.

### Requirements

All platforms require **libmpv** to be installed on your system to manage video and audio playback pipelines. Linux additionally requires **libdav1d** for AVIF image decoding.

### Platform Setup Instructions

#### Linux (.AppImage)

1. Download the latest `.AppImage` from [Releases](https://github.com/Lolle2000la/Media-Sort/releases).
2. Grant executable permissions and launch:
   ```bash
   chmod +x MediaSort-Linux.AppImage
   ./MediaSort-Linux.AppImage
   ```

#### Windows (Setup.exe)

1. Download the standalone `Setup.exe` from [Releases](https://github.com/Lolle2000la/Media-Sort/releases).
2. Run the installer wrapper to deploy the application to your local system.

#### macOS (.app Bundle)

Because this open-source variant is published without a paid Apple Developer ID signature, macOS will flag the application bundle as untrusted under Gatekeeper rules.

**To install and execute:**

1. Download and extract the `.zip` archive from [Releases](https://github.com/Lolle2000la/Media-Sort/releases).
2. Drag `Media Sort.app` to your `/Applications` directory.
3. Open a terminal and strip the quarantine flag appended by your browser:
   ```bash
   xattr -cr /Applications/Media\ Sort.app
   ```
4. Alternatively, try launching the program normally. When blocked, navigate through **System Settings > Privacy & Security**, scroll to the **Security** section, and click **Open Anyway**.
