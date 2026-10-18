#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;

#[cfg(feature = "demo")]
mod demo;
#[cfg(target_os = "linux")]
mod linux_desktop;
mod message;
mod state;
mod subscriptions;
mod update;
mod view;
mod widgets;

use iced::window;
use media_sort_core::settings::store::SettingsStore;
#[cfg(feature = "velopack")]
pub mod updater;

fn initialize_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        let payload = panic_info
            .payload()
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| {
                panic_info
                    .payload()
                    .downcast_ref::<String>()
                    .map(|s| s.as_str())
            })
            .unwrap_or("Unknown panic payload");

        let location = panic_info
            .location()
            .map(|loc| format!("{}:{}:{}", loc.file(), loc.line(), loc.column()))
            .unwrap_or_else(|| "unknown location".to_string());

        let crash_log = format!(
            "--- MEDIA SORT CRASH LOG ---\n\
             Location: {}\n\
             Cause: {}\n\
             Backtrace:\n{}\n",
            location,
            payload,
            std::backtrace::Backtrace::force_capture(),
        );

        if let Some(mut log_path) = dirs::data_local_dir() {
            log_path.push("media-sort");
            log_path.push("crashes");
            let _ = std::fs::create_dir_all(&log_path);
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs().to_string())
                .unwrap_or_else(|_| "0".to_string());
            log_path.push(format!("crash_{}.log", ts));
            let _ = std::fs::write(&log_path, &crash_log);
        }

        default_hook(panic_info);
    }));
}

#[derive(clap::Parser, Debug, Clone)]
#[command(name = "media-sort", author, version, about = "A media sorter and viewer", long_about = None)]
pub struct Cli {
    /// Directory to open on startup.
    #[arg(value_name = "DIRECTORY")]
    pub directory: Option<std::path::PathBuf>,

    /// Export the demo flows as videos and exit.
    #[cfg(feature = "demo")]
    #[arg(long)]
    pub export: bool,

    /// Path to JSON spec file or directory of spec files.
    #[cfg(feature = "demo")]
    #[arg(long, default_value = "resources/demo_flows")]
    pub demo_spec: String,

    /// Output video path or directory.
    #[cfg(feature = "demo")]
    #[arg(long, default_value = "website/public/demos")]
    pub demo_export: String,

    /// Run the interactive demo mode with the given spec.
    #[cfg(feature = "demo")]
    #[arg(long)]
    pub demo: bool,
}

/// The tracing filter.
///
/// When `RUST_LOG` is set it is the whole story: the previous setup appended an
/// `info` directive *after* reading the environment, which silently overrode a
/// bare `RUST_LOG=debug` and left no way to see debug output at all.
///
/// Without `RUST_LOG` the level is `info`, except for `iced_winit`: its
/// info-level "Window attributes" line formats the whole window as a `Debug`
/// value, including the icon as `RgbaIcon { rgba: [262144 numbers] }` — one
/// line per byte, ~260k lines on every start, which buries every other message
/// (and has to be scrolled past to read anything).
fn log_filter() -> tracing_subscriber::EnvFilter {
    match std::env::var("RUST_LOG") {
        Ok(value) if !value.trim().is_empty() => tracing_subscriber::EnvFilter::try_new(value)
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        _ => tracing_subscriber::EnvFilter::new("iced_winit=warn,info"),
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    initialize_panic_hook();

    tracing_subscriber::fmt()
        .with_env_filter(log_filter())
        .init();

    #[cfg(feature = "velopack")]
    {
        updater::pre_startup_verify_packages();

        let mut app = velopack::VelopackApp::build();
        #[cfg(target_os = "windows")]
        {
            app = app.on_before_uninstall_fast_callback(|_version| {
                let _ = media_sort_backend::platform::windows_shell::unregister();
            });
        }
        app.run();
    }

    let cli = <Cli as clap::Parser>::parse();

    #[cfg(not(feature = "demo"))]
    {
        let _ = &cli;
    }

    let discovered = iced_mpv::MpvContext::query_supported_extensions();
    media_sort_core::media_type::MediaRegistry::init(discovered);

    let settings = SettingsStore::load().unwrap_or_default();

    #[cfg(feature = "demo")]
    if let Some(result) = crate::demo::try_headless_export(&cli) {
        return result;
    }

    let icon =
        window::icon::from_file_data(include_bytes!("../../../packaging/windows/icon.ico"), None)
            .ok();

    let window_settings = window::Settings {
        position: window::Position::Specific(iced::Point::new(
            settings.window_position.left as f32,
            settings.window_position.top as f32,
        )),
        size: iced::Size::new(
            settings.window_position.width as f32,
            settings.window_position.height as f32,
        ),
        icon,
        #[allow(unused_mut)]
        platform_specific: {
            let mut ps = window::settings::PlatformSpecific::default();
            #[cfg(target_os = "linux")]
            {
                // Wayland has no window-icon channel: the compositor resolves
                // the icon through the desktop entry matching this app id.
                // `application_id` adopts an existing entry for this executable
                // and otherwise provisions a hidden one plus the icon in the
                // user's XDG data directories (see the module).
                ps.application_id = linux_desktop::application_id();
            }
            ps
        },
        ..window::Settings::default()
    };

    iced::application(
        {
            let cli = cli.clone();
            move || {
                let settings = SettingsStore::load().unwrap_or_default();
                let state = crate::state::AppState::new(settings.clone());
                #[cfg(feature = "demo")]
                let mut state = state;
                let mut startup_path = None;

                #[cfg(feature = "demo")]
                if let Some(root) = crate::demo::init(&cli, &mut state) {
                    startup_path = Some(root);
                }

                if let Some(arg_path) = cli.directory.as_ref().filter(|p| p.is_dir()) {
                    startup_path = Some(arg_path.clone());
                }

                if startup_path.is_none()
                    && state.settings.general.reopen_last_opened_folder
                    && let Some(ref last_path_str) = state.settings.general.last_opened_folder
                {
                    let last_path = std::path::PathBuf::from(last_path_str);
                    if last_path.exists() {
                        startup_path = Some(last_path);
                    }
                }

                if startup_path.is_none()
                    && let Some(pic_dir) = dirs::picture_dir()
                    && pic_dir.exists()
                {
                    startup_path = Some(pic_dir);
                }

                let mut tasks = vec![];
                tasks.push(iced::Task::done(crate::message::Message::SettingsLoaded(
                    Box::new(Ok(settings.clone())),
                )));
                tasks.push(iced::system::theme().map(crate::message::Message::SystemThemeChanged));

                if let Some(path) = startup_path {
                    tasks.push(iced::Task::done(crate::message::Message::Folder(
                        crate::message::FolderMessage::Open(path),
                    )));
                }

                #[cfg(feature = "velopack")]
                if state.settings.general.check_for_updates_on_startup {
                    tasks.push(iced::Task::done(crate::message::Message::Update(
                        crate::message::UpdateMessage::CheckForUpdates,
                    )));
                }

                (state, iced::Task::batch(tasks))
            }
        },
        app::update,
        app::view,
    )
    .settings(iced::Settings {
        id: Some(String::from("MediaSort")),
        ..iced::Settings::default()
    })
    .title("Media Sort")
    .theme(app::theme)
    .subscription(app::subscription)
    .font(lucide_icons::LUCIDE_FONT_BYTES)
    .window(window_settings)
    .run()?;

    Ok(())
}
