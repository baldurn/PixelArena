//! Clips and tones mixed into one mono stream, piped to an output command
//! (pw-cat or aplay).
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
    pos: usize,
}

impl Voice {
    pub fn new(owner: u64, name: Option<String>, samples: Arc<Vec<i16>>, volume: f32, looped: bool) -> Self {
        Self { owner, name, samples, volume, looped, pos: 0 }
    }
}

pub struct Audio {
    voices: Mutex<Vec<Voice>>,
    /// False once the output command has exited
    alive: AtomicBool,
}

impl Audio {
    pub fn start(command: &[String]) -> std::io::Result<Arc<Self>> {
        let mut child = Command::new(&command[0])
            .args(&command[1..])
            .stdin(Stdio::piped())
            .spawn()?;
        let mut stdin = child.stdin.take().expect("piped stdin");
        let audio = Arc::new(Self { voices: Mutex::new(Vec::new()), alive: AtomicBool::new(true) });
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
                mix.fill(0.0);
                mixer.voices.lock().unwrap().retain_mut(|v| {
                    let mut n = 0;
                    while n < CHUNK {
                        let take = (CHUNK - n).min(v.samples.len() - v.pos);
                        for (m, &s) in mix[n..n + take].iter_mut().zip(&v.samples[v.pos..v.pos + take]) {
                            *m += s as f32 * v.volume;
                        }
                        n += take;
                        v.pos += take;
                        if v.pos >= v.samples.len() {
                            if !v.looped || v.samples.is_empty() {
                                return false;
                            }
                            v.pos = 0;
                        }
                    }
                    true
                });
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
