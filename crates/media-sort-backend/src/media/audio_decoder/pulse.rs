//! PulseAudio output for the audio player (feature `pulse`).
//!
//! cpal's only usable Linux backend is ALSA, and inside a Flatpak sandbox its
//! stream cannot deliver: the ALSA `pulse` plugin opens, the client even
//! reaches the server, and then every read fails with
//! `ALSA function 'snd_pcm_avail_delay' failed with error 'I/O error (5)'` —
//! while `ffmpeg` and `mpv` use the very same socket without trouble, and no
//! ALSA configuration (`plug` wrapper, pinned rate/format, blocking mode)
//! changes that. PulseAudio is also the only audio protocol a sandbox exposes
//! (Flatpak's `--socket=pulseaudio`; PipeWire's own socket is not there).
//!
//! So instead of letting cpal open a device, the player drives rodio's mixer
//! itself and writes the samples straight to the PulseAudio socket. Everything
//! that is not the output device still comes from rodio: decoding, queueing,
//! pause/seek/volume and the playback position are all `Player`/`Mixer`
//! behaviour, and the mixer is paced by these writes.
//!
//! The caller falls back to the default cpal device when no server answers, so
//! enabling the feature is safe on a machine without PulseAudio.

use std::num::NonZero;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use libpulse_binding::def::BufferAttr;
use libpulse_binding::sample::{Format, Spec};
use libpulse_binding::stream::Direction;
use libpulse_simple_binding::Simple;
use rodio::mixer::MixerSource;
use rodio::{ChannelCount, SampleRate};

/// Sample rate and channel count of the mixer, which the PulseAudio stream is
/// opened with: the two must agree.
pub const RATE: SampleRate = NonZero::new(48_000).expect("48000 is not zero");
/// See [`RATE`].
pub const CHANNELS: ChannelCount = NonZero::new(2).expect("2 is not zero");

/// Interleaved samples per write (20 ms of stereo audio at 48 kHz).
/// `pa_simple_write` blocks until the server has room, so this bounds both
/// output latency and how long a stop request can take to be noticed.
const CHUNK_SAMPLES: usize = 1920;

/// Target length of the server-side buffer.
///
/// PulseAudio's default is 2 seconds, and because the writer keeps producing
/// silence while idle or paused, that much audio sits ahead of whatever is
/// started or resumed next — which is what made pause/stop lag. A smaller
/// buffer trades that latency against the headroom the writer thread needs when
/// it is descheduled while thumbnails or 4K video are decoded.
const TARGET_BUFFER_MS: u32 = 150;

/// A running PulseAudio output. Dropping it stops the writer thread.
pub struct PulseOutput {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl PulseOutput {
    /// Connect to the PulseAudio server and start writing `source` to it.
    ///
    /// The connection is made on the calling thread, so an unreachable server
    /// is reported as an error (and the caller falls back to the default cpal
    /// device) instead of vanishing into a thread.
    pub fn start(source: MixerSource) -> Result<Self, String> {
        let spec = Spec {
            // float32 is what PulseAudio and PipeWire use natively, so no
            // quantisation or conversion is imposed here (this is what the
            // `f32` samples from the mixer already are).
            format: Format::F32le,
            channels: 2,
            rate: 48_000,
        };
        if !spec.is_valid() {
            return Err("invalid PulseAudio sample spec".to_string());
        }
        // Without explicit buffer attributes PulseAudio defaults `tlength` to
        // 2 seconds (384 KB); keep it at [`TARGET_BUFFER_MS`] and let the server
        // choose the remaining fields.
        let bytes_per_frame = u32::from(CHANNELS.get()) * size_of::<f32>() as u32;
        let attr = BufferAttr {
            maxlength: u32::MAX,
            tlength: (RATE.get() / 1000) * TARGET_BUFFER_MS * bytes_per_frame,
            prebuf: u32::MAX,
            minreq: u32::MAX,
            fragsize: u32::MAX,
        };
        let simple = Simple::new(
            None,
            "media-sort",
            Direction::Playback,
            None,
            "Media Sort",
            &spec,
            None,
            Some(&attr),
        )
        .map_err(|err| format!("could not connect to PulseAudio: {err}"))?;

        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name("pulse-out".to_string())
            .spawn(move || write_loop(simple, source, &thread_stop))
            .map_err(|err| format!("could not start the PulseAudio writer: {err}"))?;

        Ok(Self {
            stop,
            thread: Some(thread),
        })
    }
}

impl Drop for PulseOutput {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Pull mixed samples and write them to the server until told to stop.
fn write_loop(simple: Simple, mut source: MixerSource, stop: &AtomicBool) {
    let mut buffer: Vec<u8> = Vec::with_capacity(CHUNK_SAMPLES * size_of::<f32>());
    while !stop.load(Ordering::Relaxed) {
        buffer.clear();
        for _ in 0..CHUNK_SAMPLES {
            // Match rodio's own device stream callback: if the mixer ever has
            // no active sub-sources, emit silence rather than terminating the
            // output thread permanently. Mixed sources can also sum past full
            // scale, so clip like the device path would.
            let sample = source.next().unwrap_or(0.0).clamp(-1.0, 1.0);
            buffer.extend_from_slice(&sample.to_le_bytes());
        }
        if let Err(err) = simple.write(&buffer) {
            tracing::warn!("PulseAudio write failed: {err}");
            break;
        }
    }
}
