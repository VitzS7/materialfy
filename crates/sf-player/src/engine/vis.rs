//! Tap пост-EQ звука для «дыхания» обложки (перенос spotifast/src/vis.rs без Winamp/MilkDrop).
//!
//! The tap wraps the active sink and stores half a second of post-EQ,
//! pre-volume audio. Its level drives the cover's breathing; the volume knob
//! never moves it.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use librespot_playback::audio_backend::{Sink, SinkResult};
use librespot_playback::convert::Converter;
use librespot_playback::decoder::AudioPacket;
use librespot_playback::mixer::VolumeGetter;
use librespot_playback::{NUM_CHANNELS, SAMPLE_RATE};

/// Half a second of audio.
const KEPT: usize = SAMPLE_RATE as usize / 2;
/// How far behind the newest sample the level looks, so that it follows
/// what the speaker is playing rather than what the sink has queued.
pub const LAG: usize = SAMPLE_RATE as usize * 3 / 20;
/// Окно RMS для уровня: ~50 мс.
pub const LEVEL_SAMPLES: usize = SAMPLE_RATE as usize / 20;

/// The last half second of sound (mono mix), shared between the player's
/// thread and whoever reads the level.
pub struct AudioTap {
    samples: Mutex<VecDeque<f32>>,
    /// Нужен ли уровень (core.levels_wanted): пока нет, `Tapped` не наполняет буфер —
    /// даунмикс и блокировка на каждый пакет не нужны, пока обложка не «дышит».
    wanted: AtomicBool,
}

impl std::fmt::Debug for AudioTap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AudioTap")
    }
}

impl Default for AudioTap {
    fn default() -> Self {
        Self {
            samples: Mutex::new(VecDeque::with_capacity(KEPT)),
            wanted: AtomicBool::new(false),
        }
    }
}

impl AudioTap {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Включает наполнение буфера. При включении сбрасываем накопленное раньше:
    /// уровень не должен прыгнуть от сэмплов, отложенных до паузы чтения.
    pub fn set_wanted(&self, value: bool) {
        if !self.wanted.swap(value, Ordering::Relaxed) && value {
            self.clear();
        }
    }

    pub fn wanted(&self) -> bool {
        self.wanted.load(Ordering::Relaxed)
    }

    /// Adds scaled stereo samples to the mono buffer.
    pub fn push(&self, interleaved: &[f64], gain: f32) {
        let mut samples = self.samples.lock().unwrap_or_else(|p| p.into_inner());
        let (frames, _) = interleaved.as_chunks::<{ NUM_CHANNELS as usize }>();
        for frame in frames {
            let mono = frame.iter().sum::<f64>() as f32 / frame.len() as f32 * gain;
            if samples.len() == KEPT {
                samples.pop_front();
            }
            samples.push_back(mono);
        }
    }
    /// The `count` samples ending `lag` samples before the newest, with
    /// silence where there is less than that.
    pub fn window(&self, count: usize, lag: usize) -> Vec<f32> {
        let samples = self.samples.lock().unwrap_or_else(|p| p.into_inner());
        let end = samples.len().saturating_sub(lag);
        let start = end.saturating_sub(count);
        let mut out = vec![0.0; count];
        let taken = end - start;
        for (slot, sample) in out[count - taken..]
            .iter_mut()
            .zip(samples.range(start..end))
        {
            *slot = *sample;
        }
        out
    }

    pub fn clear(&self) {
        self.samples
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clear();
    }

    /// Уровень для Player.level: RMS последних ~50 мс (с задержкой LAG) → дБ → level_from_db (0..1).
    pub fn level(&self) -> f64 {
        let window = self.window(LEVEL_SAMPLES, LAG);
        let power = window.iter().map(|s| f64::from(*s) * f64::from(*s)).sum::<f64>()
            / window.len().max(1) as f64;
        if power <= 0.0 {
            return 0.0;
        }
        level_from_db(10.0 * power.log10())
    }
}

/// RMS-уровень (дБ) → 0..1, как в yamusic-md3: clamp((db + 42) / 36), бесконечность → 0.
pub fn level_from_db(db: f64) -> f64 {
    if db.is_finite() { ((db + 42.0) / 36.0).clamp(0.0, 1.0) } else { 0.0 }
}

/// Runs the equalizer and taps the signal before passing it to the real sink.
pub struct Tapped {
    inner: Box<dyn Sink>,
    control: Arc<super::sink::AudioControl>,
    tap: Arc<AudioTap>,
    eq: super::eq::Processor,
    /// Player volume, used to calculate the limiter ceiling.
    volume: Box<dyn VolumeGetter + Send>,
    /// Whether this wrapper applies volume after the tap. Otherwise the inner
    /// sink applies it, still after the tap and to already queued audio.
    applies_volume: bool,
    /// Final limiter, placed here because this stage knows the output volume.
    limiter: super::limiter::Limiter,
    /// Track normalization factor. The tap removes it so visualizers show the
    /// source dynamics, as Winamp's analyser did.
    normalisation: Arc<std::sync::atomic::AtomicU64>,
}

impl Tapped {
    pub fn new(
        inner: Box<dyn Sink>,
        control: Arc<super::sink::AudioControl>,
        tap: Arc<AudioTap>,
        volume: Box<dyn VolumeGetter + Send>,
        applies_volume: bool,
        eq: super::eq::SharedEq,
        normalisation: Arc<std::sync::atomic::AtomicU64>,
    ) -> Self {
        Self {
            inner,
            control,
            tap,
            eq: super::eq::Processor::new(eq),
            volume,
            applies_volume,
            limiter: super::limiter::Limiter::new(f64::from(SAMPLE_RATE)),
            normalisation,
        }
    }
}

/// Full-scale level for samples leaving `Tapped`.
///
/// This is 1.0 after volume is applied. Before volume, it is the level that
/// becomes 1.0 after the inner sink applies volume.
fn full_scale(volume: f64, applied: bool) -> Option<f64> {
    if applied {
        Some(1.0)
    } else if volume > f64::EPSILON {
        Some(1.0 / volume)
    } else {
        // At zero volume, no finite pre-volume ceiling is needed.
        None
    }
}

impl Sink for Tapped {
    fn start(&mut self) -> SinkResult<()> {
        self.inner.start()
    }

    fn stop(&mut self) -> SinkResult<()> {
        self.tap.clear();
        self.inner.stop()
    }

    fn write(&mut self, packet: AudioPacket, converter: &mut Converter) -> SinkResult<()> {
        if self.control.take_processing_reset() {
            self.eq.reset();
            self.limiter = super::limiter::Limiter::new(f64::from(SAMPLE_RATE));
            self.tap.clear();
        }
        let packet = match packet {
            AudioPacket::Samples(mut samples) => {
                self.eq.process(&mut samples);
                // Наполняем буфер, только когда уровень кому-то нужен: пока обложка не
                // «дышит» (core.levels_wanted), даунмикс половины секунды на каждый пакет —
                // работа в никуда. Один Relaxed-загрузка на пакет.
                if self.tap.wanted() {
                    // Post-EQ, pre-volume, pre-normalisation: the equalizer
                    // shapes what the bars show; the volume knob and the
                    // loudness housekeeping never move them.
                    let factor = f64::from_bits(
                        self.normalisation
                            .load(std::sync::atomic::Ordering::Relaxed),
                    );
                    let restore = if factor > 0.05 && factor < 20.0 {
                        (1.0 / factor).clamp(0.125, 8.0) as f32
                    } else {
                        1.0
                    };
                    self.tap.push(&samples, restore);
                }
                let attenuation = self.volume.attenuation_factor();
                if self.applies_volume {
                    for sample in &mut samples {
                        *sample *= attenuation;
                    }
                }
                if let Some(full_scale) = full_scale(attenuation, self.applies_volume) {
                    self.limiter.process(&mut samples, full_scale);
                }
                AudioPacket::Samples(samples)
            }
            raw => raw,
        };
        self.inner.write(packet, converter)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct RecordedSink(Arc<Mutex<Vec<f64>>>);

    impl Sink for RecordedSink {
        fn start(&mut self) -> SinkResult<()> {
            Ok(())
        }

        fn stop(&mut self) -> SinkResult<()> {
            Ok(())
        }

        fn write(&mut self, packet: AudioPacket, _: &mut Converter) -> SinkResult<()> {
            self.0
                .lock()
                .unwrap()
                .extend_from_slice(packet.samples().unwrap());
            Ok(())
        }
    }

    fn recorded_tap(
        control: Arc<crate::engine::sink::AudioControl>,
        eq: crate::engine::eq::SharedEq,
    ) -> (Tapped, Arc<Mutex<Vec<f64>>>) {
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let tapped = Tapped::new(
            Box::new(RecordedSink(Arc::clone(&recorded))),
            control,
            AudioTap::new(),
            Box::new(librespot_playback::mixer::NoOpVolume),
            false,
            eq,
            Arc::new(std::sync::atomic::AtomicU64::new(1.0f64.to_bits())),
        );
        (tapped, recorded)
    }

    fn write_level(tapped: &mut Tapped, level: f64) {
        tapped
            .write(
                AudioPacket::Samples(vec![level; 2048]),
                &mut Converter::new(None),
            )
            .unwrap();
    }

    #[test]
    fn replacing_a_track_discards_held_audio_before_its_silent_intro() {
        for equalizer_on in [false, true] {
            let control = crate::engine::sink::AudioControl::new(crate::engine::sink::DEFAULT_BUFFER_MS);
            let eq = crate::engine::eq::shared();
            {
                let mut settings = eq.lock();
                settings.on = equalizer_on;
                settings.bands_db[0] = 12.0;
            }
            let (mut tapped, recorded) = recorded_tap(Arc::clone(&control), eq);
            write_level(&mut tapped, 0.25);
            control.interrupt();
            // Old packets can still arrive while the replacement is loading.
            write_level(&mut tapped, 0.25);
            control.interrupt();
            control.track_changed();
            recorded.lock().unwrap().clear();
            write_level(&mut tapped, 0.0);
            assert!(
                recorded.lock().unwrap().iter().all(|sample| *sample == 0.0),
                "the old limiter delay and equalizer tail must not reach the new track"
            );
        }
    }

    #[test]
    fn a_confirmed_seek_discards_the_processing_history() {
        let control = crate::engine::sink::AudioControl::new(crate::engine::sink::DEFAULT_BUFFER_MS);
        let (mut tapped, recorded) = recorded_tap(Arc::clone(&control), crate::engine::eq::shared());
        write_level(&mut tapped, 0.25);
        control.handle_player_event(&librespot_playback::player::PlayerEvent::Seeked {
            play_request_id: 1,
            track_id: librespot_core::SpotifyUri::from_uri("spotify:track:14XWXWv5FoCbFzLksawpEe")
                .unwrap(),
            position_ms: 0,
        });
        recorded.lock().unwrap().clear();
        write_level(&mut tapped, 0.0);
        assert!(recorded.lock().unwrap().iter().all(|sample| *sample == 0.0));
    }

    #[test]
    fn natural_track_changes_and_pause_keep_the_limiter_tail() {
        let control = crate::engine::sink::AudioControl::new(crate::engine::sink::DEFAULT_BUFFER_MS);
        let (mut tapped, recorded) = recorded_tap(Arc::clone(&control), crate::engine::eq::shared());
        write_level(&mut tapped, 0.25);
        control.track_changed();
        tapped.stop().unwrap();
        tapped.start().unwrap();
        recorded.lock().unwrap().clear();
        write_level(&mut tapped, 0.0);
        let samples = recorded.lock().unwrap();
        assert!(samples[..706].iter().all(|sample| *sample == 0.25));
        assert!(samples[706..].iter().all(|sample| *sample == 0.0));
    }
    #[test]
    fn the_tap_buffers_samples_only_while_the_level_is_wanted() {
        let control = crate::engine::sink::AudioControl::new(crate::engine::sink::DEFAULT_BUFFER_MS);
        let (mut tapped, _recorded) = recorded_tap(Arc::clone(&control), crate::engine::eq::shared());
        // Пока уровень никому не нужен, пакеты не попадают в буфер:
        // один пакет целиком внутри LAG-окна, level() его не увидит —
        // проверяем буфер напрямую.
        assert!(!tapped.tap.wanted());
        write_level(&mut tapped, 0.25);
        assert!(tapped
            .tap
            .samples
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .is_empty());
        assert_eq!(tapped.tap.level(), 0.0);
        // Спросили уровень: буфер наполняется, но level() смотрит на окно
        // за LAG сэмплов до новейшего (LAG больше пакета) — пишем с запасом.
        tapped.tap.set_wanted(true);
        for _ in 0..8 {
            write_level(&mut tapped, 0.25);
        }
        assert!(tapped.tap.level() > 0.0);
        // Выключение и повторное включение сбрасывает буфер: отложенных
        // сэмплов до паузы чтения остаться не должно.
        tapped.tap.set_wanted(false);
        tapped.tap.set_wanted(true);
        assert!(tapped
            .tap
            .window(LEVEL_SAMPLES, 0)
            .iter()
            .all(|sample| *sample == 0.0));
    }

    #[test]
    fn the_tap_mixes_stereo_down_and_pads_with_silence() {
        let tap = AudioTap::new();
        let interleaved: Vec<f64> = vec![0.5, -0.5, 1.0, 0.0, 0.2, 0.2];
        tap.push(&interleaved, 2.0);
        assert_eq!(tap.window(5, 0), [0.0, 0.0, 0.0, 1.0, 0.4]);
        assert_eq!(tap.window(2, 1), [0.0, 1.0]);
        tap.clear();
        assert_eq!(tap.window(2, 0), [0.0, 0.0]);
    }

    #[test]
    fn the_tap_keeps_half_a_second_at_most() {
        let tap = AudioTap::new();
        let interleaved = vec![0.25f64; 2 * (KEPT + 100)];
        tap.push(&interleaved, 1.0);
        let samples = tap.samples.lock().unwrap();
        assert_eq!(samples.len(), KEPT);
    }

    #[test]
    fn full_scale_follows_the_volume_still_to_come() {
        assert_eq!(full_scale(0.5, true), Some(1.0), "already applied: one");
        assert_eq!(full_scale(0.25, false), Some(4.0), "a quarter to come");
        assert_eq!(full_scale(1.0, false), Some(1.0), "full volume to come");
        assert_eq!(full_scale(0.0, false), None, "silence has no ceiling");
    }

    #[test]
    fn the_level_is_silent_without_sound_and_rises_with_it() {
        let tap = AudioTap::new();
        assert_eq!(tap.level(), 0.0);
        tap.push(&vec![0.5f64; 2 * KEPT], 1.0);
        let loud = tap.level();
        assert!(loud > 0.9, "−6 dB is nearly full scale: {loud}");
        tap.clear();
        tap.push(&vec![0.01f64; 2 * KEPT], 1.0);
        let quiet = tap.level();
        assert!(quiet > 0.0 && quiet < loud, "{quiet}");
    }

    #[test]
    fn level_from_db_maps_the_upstream_range() {
        assert_eq!(level_from_db(f64::NEG_INFINITY), 0.0);
        assert_eq!(level_from_db(-42.0), 0.0);
        assert_eq!(level_from_db(-6.0), 1.0);
        assert!((level_from_db(-24.0) - 0.5).abs() < 1e-9);
    }
}
