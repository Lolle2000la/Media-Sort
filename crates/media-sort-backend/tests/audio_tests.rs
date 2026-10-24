use std::path::PathBuf;
use std::time::{Duration, Instant};

use media_sort_backend::media::audio_decoder::AudioPlayer;

#[test]
fn test_audio_player_new() {
    // Audio output is non-fatal by design (AppState keeps a None player):
    // headless CI runners have no sound card, so skip instead of failing.
    match AudioPlayer::new() {
        Ok(_) => {}
        Err(e) => eprintln!("SKIP: no audio device available: {e}"),
    }
}

#[test]
fn test_audio_player_plays_and_advances_position() {
    let player = match AudioPlayer::new() {
        Ok(player) => player,
        Err(e) => {
            eprintln!("SKIP: no audio device available: {e}");
            return;
        }
    };

    // Wait briefly before loading a file: if the output writer thread exited
    // when the mixer was initially idle, playback below would never advance.
    std::thread::sleep(Duration::from_millis(50));

    player.set_volume(0.0);
    let sample = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/test_audio.mp3");
    player.play(&sample).expect("play test_audio.mp3");
    assert!(player.duration() > 0.0, "duration should be populated");
    assert!(!player.empty(), "player queue should not be empty");

    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(2) && player.position() == 0.0 {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        player.position() > 0.0,
        "playback position should advance while the output stream is consuming samples"
    );

    player.stop();
    let stop_start = Instant::now();
    while stop_start.elapsed() < Duration::from_millis(500) && !player.empty() {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(player.empty());
    assert!(!player.is_paused());
    assert_eq!(player.duration(), 0.0);
}
