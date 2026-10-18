use std::fs::File;
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use rodio::{Decoder, DeviceSinkBuilder, Player, Source};

/// PulseAudio output, behind the `pulse` feature. Sandboxes expose only the
/// PulseAudio socket and cpal's ALSA stream cannot deliver through it; see the
/// module for the evidence and why this drives rodio's mixer directly.
#[cfg(all(target_os = "linux", feature = "pulse"))]
mod pulse;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AudioError {
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error("decoder error: {0}")]
    Decoder(String),
    #[error("playback error: {0}")]
    Playback(String),
    #[error("seek error: {0}")]
    Seek(String),
}

impl From<rodio::decoder::DecoderError> for AudioError {
    fn from(e: rodio::decoder::DecoderError) -> Self {
        AudioError::Decoder(e.to_string())
    }
}

impl From<rodio::PlayError> for AudioError {
    fn from(e: rodio::PlayError) -> Self {
        AudioError::Playback(e.to_string())
    }
}

impl From<rodio::DeviceSinkError> for AudioError {
    fn from(e: rodio::DeviceSinkError) -> Self {
        AudioError::Playback(e.to_string())
    }
}

impl From<rodio::source::SeekError> for AudioError {
    fn from(e: rodio::source::SeekError) -> Self {
        AudioError::Seek(e.to_string())
    }
}

pub struct AudioPlayer {
    /// Keeps the output alive for as long as the player exists.
    _output: Output,
    player: Player,
    duration: Mutex<f64>,
}

/// Where the player's samples go.
///
/// The payloads are never inspected — they exist so that the running output
/// (and, for cpal, the stream it owns) lives exactly as long as the player.
#[allow(dead_code)]
enum Output {
    /// cpal's default device (ALSA on Linux) — the normal path.
    Device(rodio::MixerDeviceSink),
    /// PulseAudio, written directly: see [`pulse`].
    #[cfg(all(target_os = "linux", feature = "pulse"))]
    Pulse(pulse::PulseOutput),
}

fn probe_duration(path: &Path) -> Option<f64> {
    use symphonia::core::formats::probe::Hint;
    use symphonia::core::formats::{FormatOptions, TrackType};
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::units::Timestamp;

    let file = File::open(path).ok()?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let format = symphonia::default::get_probe()
        .probe(
            &hint,
            mss,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .ok()?;
    let track = format.default_track(TrackType::Audio)?;
    let tb = track.time_base?;
    let frames = track.num_frames?;
    let ts = Timestamp::try_from(frames).ok()?;
    let time = tb.calc_time(ts)?;
    Some(time.as_secs_f64())
}

impl AudioPlayer {
    pub fn new() -> Result<Self, AudioError> {
        // Prefer PulseAudio when the feature is built in: inside a sandbox it
        // is the only output that works (see `pulse`), and elsewhere it is
        // whatever the desktop routes to anyway. A machine with no server
        // answers, and the default cpal device is used as before.
        #[cfg(all(target_os = "linux", feature = "pulse"))]
        if let Some(output) = Self::with_pulse() {
            return Ok(output);
        }

        let sink = DeviceSinkBuilder::open_default_sink()?;
        let player = Player::connect_new(sink.mixer());
        Ok(Self {
            _output: Output::Device(sink),
            player,
            duration: Mutex::new(0.0),
        })
    }

    /// Build a player whose output is PulseAudio, or `None` when no server
    /// answered (the caller then uses the default cpal device).
    #[cfg(all(target_os = "linux", feature = "pulse"))]
    fn with_pulse() -> Option<Self> {
        let (mixer, source) = rodio::mixer::mixer(pulse::CHANNELS, pulse::RATE);
        // Connect the player to the mixer BEFORE starting the writer thread:
        // `MixerSource::next()` returns `None` while the mixer has no input
        // sources attached, which would cause the writer thread to see an
        // empty source on its first read before `connect_new` runs.
        let player = Player::connect_new(&mixer);
        let output = match pulse::PulseOutput::start(source) {
            Ok(output) => output,
            Err(err) => {
                tracing::warn!("PulseAudio output unavailable ({err}); using the default device");
                return None;
            }
        };
        Some(Self {
            _output: Output::Pulse(output),
            player,
            duration: Mutex::new(0.0),
        })
    }

    pub fn play(&self, path: &Path) -> Result<(), AudioError> {
        let file = File::open(path)?;
        let decoder = Decoder::try_from(file)?;
        let dur = decoder
            .total_duration()
            .map(|d| d.as_secs_f64())
            .or_else(|| probe_duration(path))
            .unwrap_or(0.0);
        if !self.player.empty() {
            self.player.stop();
        }
        self.player.append(decoder);
        if let Ok(mut d) = self.duration.lock() {
            *d = dur;
        }
        self.player.play();
        Ok(())
    }

    pub fn pause(&self) {
        self.player.pause();
    }

    pub fn resume(&self) {
        self.player.play();
    }

    pub fn stop(&self) {
        if !self.player.empty() {
            self.player.stop();
        }
        self.player.play();
        if let Ok(mut d) = self.duration.lock() {
            *d = 0.0;
        }
    }

    pub fn is_paused(&self) -> bool {
        self.player.is_paused() && !self.player.empty()
    }

    pub fn empty(&self) -> bool {
        self.player.empty()
    }

    pub fn position(&self) -> f64 {
        self.player.get_pos().as_secs_f64()
    }

    pub fn duration(&self) -> f64 {
        *self.duration.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn volume(&self) -> f32 {
        self.player.volume()
    }

    pub fn set_volume(&self, vol: f32) {
        self.player.set_volume(vol);
    }

    pub fn seek(&self, pos_secs: f64) -> Result<(), AudioError> {
        let dur = Duration::from_secs_f64(pos_secs.max(0.0));
        self.player.try_seek(dur)?;
        Ok(())
    }
}
