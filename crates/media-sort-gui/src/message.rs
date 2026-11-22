use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Instant;

use media_sort_backend::filesystem::watcher::FileSystemEvent;
use media_sort_backend::media::document::DocumentPreview;
use media_sort_core::media_type::MediaType;
use media_sort_core::settings::keybindings::Key;
use media_sort_core::settings::store::SettingsStore;

#[cfg_attr(feature = "demo", iced_automation::message)]
#[derive(Debug, Clone, serde::Deserialize, iced_automation_macros::AutomationKeycapDispatch)]
pub enum Message {
    #[serde(skip_deserializing)]
    Tick(Instant),
    #[serde(skip_deserializing)]
    SettingsLoaded(Box<Result<SettingsStore, String>>),
    #[serde(skip_deserializing)]
    #[allow(dead_code)]
    MediaScanCompleted(Result<Vec<media_sort_core::models::MediaEntry>, String>),
    Quit,
    #[serde(skip_deserializing)]
    EventOccurred(iced::Event),
    OpenCredits,
    CloseCredits,
    OpenUrl(String),

    KeyCaptured(Key, bool, bool, bool),

    #[automation(dispatch)]
    Settings(SettingsMessage),
    #[automation(dispatch)]
    Folder(FolderMessage),
    #[automation(dispatch)]
    Media(MediaMessage),
    #[serde(skip_deserializing)]
    Video(iced_mpv::PlayerMessage),
    #[serde(skip_deserializing)]
    DragDrop(DragDropMessage),
    /// Filesystem watcher batch: external add/remove/rename/modify events
    /// for watched directories (the current folder plus every expanded
    /// folder-tree node). Keeps the media grid and the folder tree in sync
    /// with changes made outside the app.
    #[serde(skip_deserializing)]
    FileSystemChanged(Vec<FileSystemEvent>),

    /// OS light/dark preference resolved at startup or changed while
    /// running. Drives the `"Auto"` theme setting.
    #[serde(skip_deserializing)]
    SystemThemeChanged(iced::theme::Mode),

    #[cfg(feature = "velopack")]
    #[serde(skip_deserializing)]
    Update(UpdateMessage),
    NoOp,
}

#[derive(Debug, Clone)]
pub enum DragDropMessage {
    FileHovered(PathBuf),
    FileHoveredCancelled,
    FileDropped(PathBuf),
    ZoneHovered(crate::state::drag_drop::DragZone),
}

#[derive(Debug, Clone, serde::Deserialize, iced_automation_macros::AutomationKeycap)]
pub enum SettingsMessage {
    #[automation(keycap = "Ctrl+,\nSettings")]
    Open,
    #[automation(keycap = "Esc\nClose")]
    Close,
    #[automation(keycap = "Ctrl+D\nChange Theme")]
    SetTheme(String),
    ToggleReopenFolder,
    ToggleReopenMedia,
    #[cfg(feature = "velopack")]
    #[serde(skip_deserializing)]
    ToggleCheckForUpdates,
    #[cfg(feature = "velopack")]
    #[serde(skip_deserializing)]
    ToggleInstallPrerelease,
    #[cfg(target_os = "windows")]
    #[serde(skip_deserializing)]
    ToggleIntegrationWithWindows,
    ToggleAnimateGifs,
    ToggleSessionPinnedFolders,
    ChangeLanguage(String),
    Save,
    RestoreDefaultKeyBindings,
    OpenKeybindings,
    EditKeyBinding(usize),
    ToggleMetadataPanel,
    StartDragFolderDivider,
    StartDragMetadataDivider,
    OpenAdvanced,
    ToggleHardwareDecoding,
    ToggleShowDocumentsByDefault(bool),
    ToggleLibreOfficePreview(bool),
}

#[derive(Debug, Clone, serde::Deserialize, iced_automation_macros::AutomationKeycap)]
pub enum FolderMessage {
    #[automation(keycap = "Enter\nOpen Folder")]
    Open(PathBuf),
    Pick,
    #[serde(skip_deserializing)]
    PickResult(Option<PathBuf>),
    PickPin,
    #[serde(skip_deserializing)]
    PickPinResult(Option<PathBuf>),
    SelectedPinned(PathBuf, usize),
    DragPinnedOver(PathBuf),
    DragPinnedReleased,
    HoverPinned(PathBuf),
    HoverPinnedNone,
    #[automation(keycap = "Arrow Keys\nSelect Destination")]
    Selected(PathBuf, usize),
    #[automation(keycap = "Space\nExpand Folder")]
    ToggleExpand(PathBuf, usize),
    PinSelected,
    UnpinCurrent(PathBuf),
    MovePinnedUp(PathBuf),
    MovePinnedDown(PathBuf),
    PinShortcut(u8),
    TriggerCreate,
    CreateInputChanged(String),
    SubmitCreate(PathBuf),
    CancelCreate,
    #[serde(skip_deserializing)]
    TreeScrolled(iced::widget::scrollable::AbsoluteOffset, f32, f32),
}

#[derive(Debug, Clone, serde::Deserialize, iced_automation_macros::AutomationKeycap)]
pub enum MediaMessage {
    #[automation(keycap = "Click\nSelect Entry")]
    SelectEntry(usize),
    #[automation(keycap = "Type Query\nFilter Results")]
    SearchQueryChanged(String),
    #[automation(keycap = "I\nFocus Search")]
    SearchFocused,
    SearchBlurred,
    #[automation(keycap = "F2\nRename")]
    TriggerRename,
    RenameInputChanged(String),
    SubmitRename,
    CancelRename,
    RenameEntry(PathBuf, String),
    MoveToFolder(PathBuf),
    CopyToFolder(PathBuf),
    DeleteEntry(PathBuf),
    Undo,
    Redo,
    GoLeft,
    #[automation(keycap = "Right Arrow\nNext Image")]
    GoRight,
    #[automation(keycap = "M\nMove to Folder")]
    MoveActive,
    #[automation(keycap = "Ctrl+C\nCopy to Folder")]
    CopyActive,
    #[serde(skip_deserializing)]
    GridScrolled(iced::widget::scrollable::AbsoluteOffset, f32, f32),
    #[serde(skip_deserializing)]
    ThumbnailReady(PathBuf, u32, u32, Vec<u8>),
    #[serde(skip_deserializing)]
    ThumbnailFailed(PathBuf, crate::subscriptions::prefetch::ThumbnailError),
    #[serde(skip_deserializing)]
    ThumbnailCancelled(PathBuf),
    #[serde(skip_deserializing)]
    ImageLoaded(PathBuf, Result<(u32, u32, Vec<u8>), String>),
    #[serde(skip_deserializing)]
    MetadataLoaded(Result<BTreeMap<String, BTreeMap<String, String>>, String>),
    OpenExternal(PathBuf),
    RevealInExplorer(PathBuf),
    StopAudio,
    AudioSeek(f64),
    AudioSetVolume(f64),
    AudioToggleMute,
    AudioPlayPause,
    #[serde(skip_deserializing)]
    ToggleTypeFilter(MediaType),
    #[serde(skip_deserializing)]
    DocumentLoaded {
        path: PathBuf,
        page_index: usize,
        result: Result<DocumentPreview, String>,
    },
    DocumentPrevPage,
    DocumentNextPage,
    DocumentSelectSheet(usize),
    DocumentLinkClicked(iced::widget::markdown::Uri),
}

#[cfg(feature = "velopack")]
#[derive(Debug, Clone)]
pub enum UpdateMessage {
    CheckForUpdates,
    UpdateAvailable(Box<velopack::UpdateInfo>),
    NoUpdateFound,
    UserConfirmedUpdate(Box<velopack::UpdateInfo>),
    UpdateFailed(String),
    DismissUpdatePrompt,
}
