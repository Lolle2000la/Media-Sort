use crate::start_video_worker_with;
use crate::state::PlayerHandle;
use iced::Subscription;
use iced::futures::SinkExt;
use mpv_utils::{PlayerConfig, VideoEvent as WorkerEvent};

#[derive(Clone)]
struct Handlers<FReady, FEvent> {
    config: PlayerConfig,
    on_ready: FReady,
    on_event: FEvent,
}

// iced identifies subscriptions by the `Hash` of their state. Closures are not
// hashable, so the identity is derived from the closure *types* via TypeId,
// plus the worker [`PlayerConfig`] — the config is part of the identity so a
// changed config restarts the worker instead of silently keeping the old one.
impl<FReady: 'static, FEvent: 'static> std::hash::Hash for Handlers<FReady, FEvent> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::any::TypeId::of::<Self>().hash(state);
        self.config.max_frame_width.hash(state);
        self.config.max_frame_height.hash(state);
        // hwdec and zero-copy change how the worker is created and how frames
        // are rendered, so toggling them must restart the worker too.
        self.config.enable_hwdec.hash(state);
        self.config.enable_zero_copy.hash(state);
    }
}

/// Spawns the video worker as an iced subscription with the default
/// [`PlayerConfig`] and maps its messages into `Message` via the two handler
/// closures.
///
/// - `on_ready` receives the opaque [`PlayerHandle`] once, when the worker is
///   up (deliver it to your [`crate::VideoState`] via
///   [`crate::PlayerMessage::Ready`]).
/// - `on_event` receives every raw worker event ([`crate::WorkerEvent`]).
///
/// Prefer [`crate::VideoPlayer::subscription`] / `subscription_with` unless
/// you need the raw closures or a custom config.
///
/// # Subscription identity
///
/// iced deduplicates subscriptions by hashing their state. Closures are not
/// hashable, so this subscription's identity is the closure *types* (via
/// `TypeId`) **plus the worker [`PlayerConfig`]**. Consequences to be aware of:
///
/// - Captured values (other than the config) do **not** restart the stream
///   when they change — the worker keeps running as long as the same closure
///   types and config are passed.
/// - Changing the config (e.g. `max_frame_width`) **does** restart the
///   worker, so the new config is actually applied.
/// - Two subscriptions with identical closure types **and** an identical
///   config are deduplicated into **one** stream by iced. If your app embeds
///   multiple video players, give each a distinct closure type.
pub fn video_player_subscription<Message, FReady, FEvent>(
    on_ready: FReady,
    on_event: FEvent,
) -> Subscription<Message>
where
    Message: Send + 'static,
    FReady: Fn(PlayerHandle) -> Message + Send + Sync + 'static + Clone,
    FEvent: Fn(WorkerEvent) -> Message + Send + Sync + 'static + Clone,
{
    video_player_subscription_with(PlayerConfig::default(), on_ready, on_event)
}

/// Like [`video_player_subscription`], with a custom [`PlayerConfig`] (e.g. a
/// different maximum frame size).
pub fn video_player_subscription_with<Message, FReady, FEvent>(
    config: PlayerConfig,
    on_ready: FReady,
    on_event: FEvent,
) -> Subscription<Message>
where
    Message: Send + 'static,
    FReady: Fn(PlayerHandle) -> Message + Send + Sync + 'static + Clone,
    FEvent: Fn(WorkerEvent) -> Message + Send + Sync + 'static + Clone,
{
    Subscription::run_with(
        Handlers {
            config,
            on_ready,
            on_event,
        },
        |handlers| {
            video_stream(
                handlers.config.clone(),
                handlers.on_ready.clone(),
                handlers.on_event.clone(),
            )
        },
    )
}

fn video_stream<Message, FReady, FEvent>(
    config: PlayerConfig,
    on_ready: FReady,
    on_event: FEvent,
) -> impl iced::futures::Stream<Item = Message>
where
    Message: Send + 'static,
    FReady: Fn(PlayerHandle) -> Message + Send + Sync + 'static,
    FEvent: Fn(WorkerEvent) -> Message + Send + Sync + 'static,
{
    iced::stream::channel(
        32,
        move |mut output: iced::futures::channel::mpsc::Sender<Message>| async move {
            let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel(64);
            let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(32);

            start_video_worker_with(cmd_rx, event_tx, config);

            if output
                .send(on_ready(PlayerHandle::from_sender(cmd_tx)))
                .await
                .is_err()
            {
                return;
            }

            while let Some(event) = event_rx.recv().await {
                if output.send(on_event(event)).await.is_err() {
                    break;
                }
            }
        },
    )
}
