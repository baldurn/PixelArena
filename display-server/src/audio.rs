//! Clips and tones mixed into one mono stream. Two ways out:
//!
//! - Linux: piped to an output command (pw-cat or aplay), see `start`.
//! - macOS and Windows: the system's own sound output through cpal, see
//!   `start_native`. The sound card's rate is usually not SAMPLE_RATE, so the
//!   mixer steps through the clips at the ratio between the two.
//!
//! The mixer keeps only a little sound queued in the pipe (QUEUE_TARGET), so a
//! new sound is heard almost at once: it waits while the pipe holds more. Left
//! to block on a full pipe instead, it would run up to 64 KiB, 1.5 s, ahead.

use std::io::{Cursor, Write};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

pub const SAMPLE_RATE: u32 = 22050;
pub const MAX_CLIP_SECONDS: u32 = 30;
pub const WAVES: [&str; 5] = ["sine", "square", "triangle", "saw", "noise"];
const CHUNK: usize = SAMPLE_RATE as usize / 50; // 20 ms
/// Bytes of sound kept queued for the output command: 2 chunks, 40 ms
const QUEUE_TARGET: usize = CHUNK * 2 * 2;

/// Bytes waiting in the pipe, None if this system cannot tell
#[cfg(target_os = "linux")]
fn queued(pipe: &std::process::ChildStdin) -> Option<usize> {
    use std::os::fd::AsRawFd;
    use std::os::raw::{c_int, c_ulong};
    extern "C" {
        fn ioctl(fd: c_int, request: c_ulong, ...) -> c_int;
    }
    const FIONREAD: c_ulong = 0x541b;
    let mut n: c_int = 0;
    // SAFETY: FIONREAD writes one int; the fd stays open for the call
    let r = unsafe { ioctl(pipe.as_raw_fd(), FIONREAD, &mut n as *mut c_int) };
    (r == 0).then_some(n.max(0) as usize)
}

#[cfg(not(target_os = "linux"))]
fn queued(_pipe: &std::process::ChildStdin) -> Option<usize> {
    None
}

pub struct Voice {
    pub owner: u64,
    pub name: Option<String>,
    samples: Arc<Vec<i16>>,
    volume: f32,
    looped: bool,
    /// In samples of `samples`; fractional when the output rate differs
    pos: f64,
}

impl Voice {
    pub fn new(owner: u64, name: Option<String>, samples: Arc<Vec<i16>>, volume: f32, looped: bool) -> Self {
        Self { owner, name, samples, volume, looped, pos: 0.0 }
    }
}

pub struct Audio {
    voices: Mutex<Vec<Voice>>,
    /// False once the output command has exited
    alive: AtomicBool,
    /// Silent output; sounds still run their course, so none resume late
    muted: AtomicBool,
}

impl Audio {
    pub fn start(command: &[String]) -> std::io::Result<Arc<Self>> {
        let mut child = Command::new(&command[0])
            .args(&command[1..])
            .stdin(Stdio::piped())
            .spawn()?;
        let mut stdin = child.stdin.take().expect("piped stdin");
        let audio = Arc::new(Self { voices: Mutex::new(Vec::new()), alive: AtomicBool::new(true), muted: AtomicBool::new(false) });
        let mixer = audio.clone();
        std::thread::spawn(move || {
            let mut mix = [0f32; CHUNK];
            let mut out = Vec::with_capacity(CHUNK * 2);
            let debug = std::env::var_os("AUDIO_DEBUG").is_some();
            let mut last_report = std::time::Instant::now();
            let (started, mut made) = (std::time::Instant::now(), 0usize);
            loop {
                // Mix at the last moment, so new sounds start soon
                if let Some(q) = queued(&stdin) {
                    if debug && last_report.elapsed().as_secs() >= 2 {
                        last_report = std::time::Instant::now();
                        eprintln!("audio: {:.0} ms queued, {:.2} s of sound made in {:.2} s", q as f64 / 2.0 / SAMPLE_RATE as f64 * 1000.0,
                                  made as f64 / SAMPLE_RATE as f64, started.elapsed().as_secs_f64());
                    }
                    if q >= QUEUE_TARGET {
                        std::thread::sleep(std::time::Duration::from_millis(5));
                        continue;
                    }
                }
                mixer.mix(&mut mix, 1.0);
                made += CHUNK;
                out.clear();
                for &m in &mix {
                    out.extend_from_slice(&(m.clamp(-32768.0, 32767.0) as i16).to_le_bytes());
                }
                if let Err(e) = stdin.write_all(&out).and_then(|_| stdin.flush()) {
                    eprintln!("audio output stopped: {e}");
                    mixer.alive.store(false, Ordering::Relaxed);
                    let _ = child.wait();
                    return;
                }
            }
        });
        Ok(audio)
    }

    /// Mixes the playing voices into `out` (i16 range), stepping `step`
    /// clip samples per output sample: 1.0 at SAMPLE_RATE
    fn mix(&self, out: &mut [f32], step: f64) {
        out.fill(0.0);
        self.voices.lock().unwrap().retain_mut(|v| {
            let n = v.samples.len();
            if n == 0 {
                return false;
            }
            for o in out.iter_mut() {
                if v.pos >= n as f64 {
                    if !v.looped {
                        return false;
                    }
                    v.pos -= n as f64;
                }
                // Linear between neighbouring samples, so other rates sound clean
                let i = v.pos as usize;
                let next = match (i + 1 < n, v.looped) {
                    (true, _) => v.samples[i + 1],
                    (false, true) => v.samples[0],
                    (false, false) => 0,
                } as f32;
                let f = (v.pos - i as f64) as f32;
                *o += (v.samples[i] as f32 * (1.0 - f) + next * f) * v.volume;
                v.pos += step;
            }
            v.looped || v.pos < n as f64
        });
        if self.muted() {
            out.fill(0.0);
        }
    }

    pub fn muted(&self) -> bool {
        self.muted.load(Ordering::Relaxed)
    }

    pub fn set_muted(&self, muted: bool) {
        self.muted.store(muted, Ordering::Relaxed);
    }

    /// Plays through the system's default sound output (WASAPI on Windows,
    /// CoreAudio on macOS)
    #[cfg(not(target_os = "linux"))]
    pub fn start_native() -> Result<Arc<Self>, String> {
        use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

        fn stream<T: cpal::SizedSample + cpal::FromSample<f32>>(
            device: &cpal::Device,
            config: &cpal::StreamConfig,
            audio: Arc<Audio>,
        ) -> Result<cpal::Stream, cpal::BuildStreamError> {
            let channels = config.channels as usize;
            let step = SAMPLE_RATE as f64 / config.sample_rate.0 as f64;
            let mut mono = Vec::new();
            device.build_output_stream(
                config,
                move |data: &mut [T], _| {
                    mono.resize(data.len() / channels, 0.0);
                    audio.mix(&mut mono, step);
                    for (frame, &m) in data.chunks_mut(channels).zip(&mono) {
                        frame.fill(T::from_sample((m / 32768.0).clamp(-1.0, 1.0)));
                    }
                },
                |e| eprintln!("audio output: {e}"),
                None,
            )
        }

        let audio = Arc::new(Self { voices: Mutex::new(Vec::new()), alive: AtomicBool::new(true), muted: AtomicBool::new(false) });
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let mixer = audio.clone();
        // The stream must stay in the thread that made it on some systems, so
        // that thread keeps it, and the sound, alive for good
        std::thread::spawn(move || {
            let started = (|| -> Result<cpal::Stream, String> {
                let device = cpal::default_host().default_output_device().ok_or("no sound output device")?;
                let supported = device.default_output_config().map_err(|e| e.to_string())?;
                let format = supported.sample_format();
                let config: cpal::StreamConfig = supported.into();
                let s = match format {
                    cpal::SampleFormat::F32 => stream::<f32>(&device, &config, mixer),
                    cpal::SampleFormat::I16 => stream::<i16>(&device, &config, mixer),
                    cpal::SampleFormat::U16 => stream::<u16>(&device, &config, mixer),
                    cpal::SampleFormat::I32 => stream::<i32>(&device, &config, mixer),
                    f => return Err(format!("sample format {f} is not supported")),
                }
                .map_err(|e| e.to_string())?;
                s.play().map_err(|e| e.to_string())?;
                Ok(s)
            })();
            match started {
                Ok(s) => {
                    let _ = ready_tx.send(Ok(()));
                    let _keep = s;
                    loop {
                        std::thread::park();
                    }
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                }
            }
        });
        ready_rx.recv().map_err(|e| e.to_string())??;
        Ok(audio)
    }

    pub fn alive(&self) -> bool {
        self.alive.load(Ordering::Relaxed)
    }

    pub fn play(&self, voice: Voice) {
        self.voices.lock().unwrap().push(voice);
    }

    /// Stops the owner's sounds named `name`, or all of them
    pub fn stop(&self, owner: u64, name: Option<&str>) {
        self.voices
            .lock()
            .unwrap()
            .retain(|v| v.owner != owner || (name.is_some() && v.name.as_deref() != name));
    }
}

/// Any 8- or 16-bit PCM WAV -> mono samples at SAMPLE_RATE
pub fn decode_wav(data: &[u8]) -> Result<Vec<i16>, String> {
    let reader = hound::WavReader::new(Cursor::new(data)).map_err(|e| format!("not a WAV file: {e}"))?;
    let spec = reader.spec();
    if spec.sample_format != hound::SampleFormat::Int || !matches!(spec.bits_per_sample, 8 | 16) {
        return Err("only 8-bit or 16-bit PCM WAV is supported".into());
    }
    if reader.duration() > spec.sample_rate * MAX_CLIP_SECONDS {
        return Err(format!("clip is longer than {MAX_CLIP_SECONDS} s"));
    }
    let shift = 16 - spec.bits_per_sample;
    let samples: Vec<i32> = reader
        .into_samples::<i32>()
        .map(|s| s.map(|s| s << shift))
        .collect::<Result<_, _>>()
        .map_err(|e| format!("bad WAV data: {e}"))?;
    let channels = spec.channels.max(1) as usize;
    let mono: Vec<i16> = samples
        .chunks(channels)
        .map(|c| (c.iter().sum::<i32>() / c.len() as i32) as i16)
        .collect();
    if spec.sample_rate == SAMPLE_RATE {
        return Ok(mono);
    }
    let n = mono.len() as u64 * SAMPLE_RATE as u64 / spec.sample_rate as u64;
    Ok((0..n)
        .map(|i| mono[(i * spec.sample_rate as u64 / SAMPLE_RATE as u64) as usize])
        .collect())
}

/// A generated sound effect; `to_freq` glides the pitch linearly over the tone.
pub fn make_tone(wave: &str, freq: f64, ms: f64, to_freq: Option<f64>, seed: u64) -> Vec<i16> {
    let n = (SAMPLE_RATE as f64 * ms / 1000.0) as usize;
    let fade = (n / 2).min(SAMPLE_RATE as usize / 200) as f64; // 5 ms ramps avoid clicks
    let to_freq = to_freq.unwrap_or(freq);
    let mut rng = seed | 1;
    let mut phase = 0.0f64;
    (0..n)
        .map(|i| {
            phase = (phase + (freq + (to_freq - freq) * i as f64 / n as f64) / SAMPLE_RATE as f64) % 1.0;
            let v = match wave {
                "sine" => (2.0 * std::f64::consts::PI * phase).sin(),
                "square" => if phase < 0.5 { 0.6 } else { -0.6 },
                "triangle" => 4.0 * (phase - 0.5).abs() - 1.0,
                "saw" => (2.0 * phase - 1.0) * 0.7,
                _ => {
                    // xorshift: noise does not need a real random generator
                    rng ^= rng << 13;
                    rng ^= rng >> 7;
                    rng ^= rng << 17;
                    (rng >> 11) as f64 / (1u64 << 53) as f64 * 1.2 - 0.6
                }
            };
            let ramp = if fade > 0.0 { (i as f64 / fade).min((n - 1 - i) as f64 / fade).min(1.0) } else { 1.0 };
            (v * ramp * 16000.0) as i16
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn audio_with(samples: Vec<i16>, looped: bool) -> Audio {
        let audio = Audio { voices: Mutex::new(Vec::new()), alive: AtomicBool::new(true), muted: AtomicBool::new(false) };
        audio.play(Voice::new(1, None, Arc::new(samples), 1.0, looped));
        audio
    }

    #[test]
    fn mixes_at_native_rate() {
        let audio = audio_with(vec![100, 200, 300], false);
        let mut out = [0f32; 5];
        audio.mix(&mut out, 1.0);
        assert_eq!(out, [100.0, 200.0, 300.0, 0.0, 0.0]);
        assert!(audio.voices.lock().unwrap().is_empty(), "a finished clip is dropped");
    }

    #[test]
    fn resamples_to_other_rates() {
        // A 100 ms tone played at 48 kHz lasts 100 ms: 4800 output samples
        let tone = make_tone("sine", 440.0, 100.0, None, 1);
        let audio = audio_with(tone.clone(), false);
        let step = SAMPLE_RATE as f64 / 48000.0;
        let mut out = vec![0f32; 6000];
        audio.mix(&mut out, step);
        let last = out.iter().rposition(|&s| s != 0.0).unwrap();
        assert!((4795..=4800).contains(&last), "tone ends at output sample {last}");
        // Smooth: between two output samples the wave moves no more than it does in the clip
        let max_clip = tone.windows(2).map(|w| (w[1] as f32 - w[0] as f32).abs()).fold(0.0, f32::max);
        let max_out = out.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0, f32::max);
        assert!(max_out <= max_clip, "{max_out} > {max_clip}");
    }

    #[test]
    fn mute_silences_but_sounds_run_on() {
        let audio = audio_with(vec![100, 200, 300, 400], false);
        audio.set_muted(true);
        let mut out = [1f32; 2];
        audio.mix(&mut out, 1.0);
        assert_eq!(out, [0.0, 0.0]);
        audio.set_muted(false);
        audio.mix(&mut out, 1.0);
        assert_eq!(out, [300.0, 400.0], "carries on where it got to, not from the start");
    }

    #[test]
    fn loops_without_a_gap() {
        let audio = audio_with(vec![1000, 2000], true);
        let mut out = [0f32; 6];
        audio.mix(&mut out, 1.0);
        assert_eq!(out, [1000.0, 2000.0, 1000.0, 2000.0, 1000.0, 2000.0]);
    }
}
