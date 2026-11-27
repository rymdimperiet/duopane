//! Lister: playing music and video.
//!
//! Music is decoded in-process (rodio/symphonia: mp3, flac, ogg, wav, m4a/aac...).
//! Anything else, and all video, goes through `ffmpeg`: video frames are piped in as
//! raw RGBA pictures and the sound as raw samples. `ffprobe` supplies length and tags.

use crate::lister_load::have;
use rodio::{DeviceSinkBuilder, MixerDeviceSink, Player as Sink, Source};
use std::num::NonZero;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

#[derive(Default, Clone)]
pub struct MediaInfo {
    pub duration: Option<f64>,
    pub width: u32,
    pub height: u32,
    pub has_video: bool,
    pub has_audio: bool,
    pub has_cover: bool,
    pub tags: Vec<(String, String)>,
    pub summary: String,
}

/// Reads length, picture size, tags and codecs with ffprobe.
pub fn probe(path: &Path) -> Option<MediaInfo> {
    if !have("ffprobe") {
        return None;
    }
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-print_format", "json", "-show_format", "-show_streams"])
        .arg(path)
        .output()
        .ok()?;
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    let mut info = MediaInfo::default();
    let mut codecs = Vec::new();
    for s in v["streams"].as_array().into_iter().flatten() {
        let codec = s["codec_name"].as_str().unwrap_or("?").to_string();
        match s["codec_type"].as_str() {
            Some("video") if s["disposition"]["attached_pic"].as_i64() == Some(1) => info.has_cover = true,
            Some("video") if !info.has_video => {
                info.has_video = true;
                info.width = s["width"].as_u64().unwrap_or(0) as u32;
                info.height = s["height"].as_u64().unwrap_or(0) as u32;
                // Phone videos are often stored sideways with a "rotate" note; ffmpeg turns them upright.
                let rot = s["tags"]["rotate"].as_str().and_then(|r| r.parse::<i64>().ok()).or_else(|| {
                    s["side_data_list"].as_array()?.iter().find_map(|d| d["rotation"].as_i64())
                });
                if matches!(rot.map(|r| r.rem_euclid(180)), Some(90)) {
                    std::mem::swap(&mut info.width, &mut info.height);
                }
                codecs.push(format!("{codec} {}×{}", info.width, info.height));
            }
            Some("audio") if !info.has_audio => {
                info.has_audio = true;
                let ch = s["channels"].as_u64().unwrap_or(0);
                let rate = s["sample_rate"].as_str().unwrap_or("");
                codecs.push(format!("{codec} {rate} Hz {}", if ch == 1 { "mono" } else { "stereo" }));
            }
            _ => {}
        }
    }
    let f = &v["format"];
    info.duration = f["duration"].as_str().and_then(|d| d.parse().ok());
    if let Some(br) = f["bit_rate"].as_str().and_then(|b| b.parse::<u64>().ok()) {
        codecs.push(format!("{} kbit/s", br / 1000));
    }
    info.summary = codecs.join(" · ");
    if let Some(tags) = f["tags"].as_object() {
        for key in ["title", "artist", "album", "album_artist", "date", "genre", "track", "comment"] {
            if let Some((_, val)) = tags.iter().find(|(k, _)| k.eq_ignore_ascii_case(key)) {
                if let Some(s) = val.as_str() {
                    let label = key.replace('_', " ");
                    let label = label[..1].to_uppercase() + &label[1..];
                    info.tags.push((label, s.to_string()));
                }
            }
        }
    }
    Some(info)
}

/// Extracts the cover picture embedded in a music file (PNG bytes).
pub fn cover_art(path: &Path) -> Option<Vec<u8>> {
    let out = Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-i"])
        .arg(path)
        .args(["-an", "-map", "0:v:0", "-frames:v", "1", "-f", "image2pipe", "-vcodec", "png", "-"])
        .output()
        .ok()?;
    (out.status.success() && !out.stdout.is_empty()).then_some(out.stdout)
}

/// Raw 16-bit stereo samples from an ffmpeg pipe.
struct PipeSource {
    r: BufReader<ChildStdout>,
}

impl Iterator for PipeSource {
    type Item = rodio::Sample;
    fn next(&mut self) -> Option<rodio::Sample> {
        let mut b = [0u8; 2];
        self.r.read_exact(&mut b).ok()?;
        Some(rodio::Sample::from(i16::from_le_bytes(b)) / 32768.0)
    }
}

impl Source for PipeSource {
    fn current_span_len(&self) -> Option<usize> {
        None
    }
    fn channels(&self) -> rodio::ChannelCount {
        NonZero::new(2).unwrap()
    }
    fn sample_rate(&self) -> rodio::SampleRate {
        NonZero::new(44100).unwrap()
    }
    fn total_duration(&self) -> Option<Duration> {
        None
    }
}

pub struct Player {
    path: PathBuf,
    pub info: MediaInfo,
    pub video: bool,
    out: Option<MixerDeviceSink>,
    sink: Option<Sink>,
    /// Music decoded in-process (seekable without restarting).
    native: bool,
    procs: Vec<Child>,
    frames: Option<Receiver<Vec<u8>>>,
    pub frame_size: [usize; 2],
    offset: f64,
    started: Option<Instant>,
    pub playing: bool,
    volume: f32,
    pub error: Option<String>,
}

impl Player {
    pub fn new(path: &Path, video: bool, volume: f32) -> Player {
        let ffmpeg = have("ffmpeg");
        let mut info = probe(path).unwrap_or_default();
        let out = DeviceSinkBuilder::open_default_sink().ok().map(|mut s| {
            // It would otherwise say so on the terminal each time the lister closes.
            s.log_on_drop(false);
            s
        });
        let mut error = None;
        if out.is_none() {
            error = Some("No sound output found; playing without sound.".to_string());
        }
        let native = !video && rodio::Decoder::new(BufReader::new(match File::open(path) {
            Ok(f) => f,
            Err(e) => {
                return Player::failed(path, video, format!("Can't open the file: {e}"));
            }
        }))
        .map(|d| {
            if info.duration.is_none() {
                info.duration = d.total_duration().map(|d| d.as_secs_f64());
            }
            info.has_audio = true;
        })
        .is_ok();
        let mut frame_size = [0, 0];
        if video {
            if !ffmpeg || info.width == 0 {
                return Player::failed(path, video, "Video preview needs ffmpeg (with ffprobe) installed.".into());
            }
            // Scale big videos down to at most 1280×720 to keep it light.
            let s = (1280.0 / info.width as f64).min(720.0 / info.height as f64).min(1.0);
            let w = ((info.width as f64 * s) as usize).max(2) & !1;
            let h = ((info.height as f64 * s) as usize).max(2) & !1;
            frame_size = [w, h];
        } else if !native && !ffmpeg {
            return Player::failed(path, video, "This audio format needs ffmpeg installed.".into());
        }
        let mut p = Player {
            path: path.to_path_buf(),
            info,
            video,
            out,
            sink: None,
            native,
            procs: Vec::new(),
            frames: None,
            frame_size,
            offset: 0.0,
            started: None,
            playing: false,
            volume,
            error,
        };
        p.start_at(0.0);
        p
    }

    fn failed(path: &Path, video: bool, msg: String) -> Player {
        Player {
            path: path.to_path_buf(),
            info: MediaInfo::default(),
            video,
            out: None,
            sink: None,
            native: false,
            procs: Vec::new(),
            frames: None,
            frame_size: [0, 0],
            offset: 0.0,
            started: None,
            playing: false,
            volume: 1.0,
            error: Some(msg),
        }
    }

    pub fn usable(&self) -> bool {
        self.native || self.frame_size[0] > 0 || (self.info.has_audio && have("ffmpeg"))
    }

    pub fn duration(&self) -> Option<f64> {
        self.info.duration
    }

    pub fn position(&self) -> f64 {
        let p = match (&self.sink, self.native) {
            (Some(s), true) => s.get_pos().as_secs_f64(),
            _ => match self.started {
                Some(t) => self.offset + t.elapsed().as_secs_f64(),
                None => self.offset,
            },
        };
        match self.info.duration {
            Some(d) => p.min(d),
            None => p,
        }
    }

    fn stop(&mut self) {
        if let Some(s) = self.sink.take() {
            s.stop();
        }
        for mut c in self.procs.drain(..) {
            let _ = c.kill();
            let _ = c.wait();
        }
        self.frames = None;
    }

    fn start_at(&mut self, t: f64) {
        self.stop();
        self.offset = t;
        if let Some(out) = &self.out {
            let h = out.mixer();
            if self.native {
                if let Ok(f) = File::open(&self.path) {
                    if let Ok(dec) = rodio::Decoder::new(BufReader::new(f)) {
                        let sink = Sink::connect_new(h);
                        sink.set_volume(self.volume);
                        sink.append(dec);
                        if t > 0.0 {
                            let _ = sink.try_seek(Duration::from_secs_f64(t));
                        }
                        self.sink = Some(sink);
                    }
                }
            } else if self.info.has_audio && have("ffmpeg") {
                let child = Command::new("ffmpeg")
                    .args(["-nostdin", "-v", "error", "-ss", &format!("{t:.3}"), "-i"])
                    .arg(&self.path)
                    .args(["-vn", "-sn", "-ac", "2", "-ar", "44100", "-f", "s16le", "-"])
                    .stdin(Stdio::null())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null())
                    .spawn();
                if let Ok(mut c) = child {
                    if let Some(out) = c.stdout.take() {
                        let sink = Sink::connect_new(h);
                        sink.set_volume(self.volume);
                        sink.append(PipeSource { r: BufReader::with_capacity(1 << 16, out) });
                        self.sink = Some(sink);
                    }
                    self.procs.push(c);
                }
            }
        }
        if self.video {
            self.spawn_frames(t, false);
        }
        self.started = Some(Instant::now());
        self.playing = true;
    }

    /// Starts ffmpeg sending video pictures; `still` grabs just one (for seeking while paused).
    fn spawn_frames(&mut self, t: f64, still: bool) {
        let [w, h] = self.frame_size;
        let mut cmd = Command::new("ffmpeg");
        cmd.args(["-nostdin", "-v", "error"]);
        if !still {
            cmd.arg("-re"); // deliver pictures at the video's own speed
        }
        cmd.args(["-ss", &format!("{t:.3}"), "-i"]).arg(&self.path);
        cmd.args(["-an", "-sn", "-vf", &format!("scale={w}:{h}"), "-pix_fmt", "rgba", "-f", "rawvideo"]);
        if still {
            cmd.args(["-frames:v", "1"]);
        }
        cmd.arg("-").stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
        let Ok(mut child) = cmd.spawn() else {
            self.error = Some("Couldn't start ffmpeg.".into());
            return;
        };
        let Some(mut out) = child.stdout.take() else { return };
        let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(3);
        let n = w * h * 4;
        std::thread::spawn(move || loop {
            let mut buf = vec![0u8; n];
            if out.read_exact(&mut buf).is_err() {
                break;
            }
            match tx.try_send(buf) {
                Ok(()) | Err(mpsc::TrySendError::Full(_)) => {} // drop a picture if the window is behind
                Err(mpsc::TrySendError::Disconnected(_)) => break,
            }
        });
        self.procs.push(child);
        self.frames = Some(rx);
    }

    pub fn toggle(&mut self) {
        if self.playing {
            self.offset = self.position();
            if self.native {
                if let Some(s) = &self.sink {
                    s.pause();
                }
            } else {
                self.stop();
            }
            self.started = None;
            self.playing = false;
        } else {
            let at = if self.info.duration.is_some_and(|d| self.offset >= d - 0.05) { 0.0 } else { self.offset };
            match &self.sink {
                Some(s) if self.native && at > 0.0 => {
                    s.play();
                    self.playing = true;
                }
                _ => self.start_at(at),
            }
        }
    }

    pub fn seek(&mut self, to: f64) {
        let to = match self.info.duration {
            Some(d) => to.clamp(0.0, (d - 0.1).max(0.0)),
            None => to.max(0.0),
        };
        if self.native {
            if let Some(s) = &self.sink {
                if s.try_seek(Duration::from_secs_f64(to)).is_ok() {
                    self.offset = to;
                    return;
                }
            }
        }
        if self.playing {
            self.start_at(to);
        } else {
            self.offset = to;
            if self.video {
                self.stop();
                self.spawn_frames(to, true);
            }
        }
    }

    pub fn set_volume(&mut self, v: f32) {
        self.volume = v.clamp(0.0, 1.0);
        if let Some(s) = &self.sink {
            s.set_volume(self.volume);
        }
    }

    /// Returns the newest video picture, and notices when playback has ended.
    pub fn poll(&mut self) -> Option<Vec<u8>> {
        let mut latest = None;
        let mut video_done = !self.video;
        if let Some(rx) = &self.frames {
            loop {
                match rx.try_recv() {
                    Ok(f) => latest = Some(f),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        video_done = true;
                        break;
                    }
                }
            }
        } else {
            video_done = true;
        }
        let audio_done = self.sink.as_ref().is_none_or(|s| s.empty());
        let past_end = self.info.duration.is_some_and(|d| self.position() >= d - 0.05);
        if self.playing && video_done && (audio_done || past_end) && self.started.is_some_and(|t| t.elapsed() > Duration::from_millis(500)) {
            self.offset = self.info.duration.unwrap_or_else(|| self.position());
            self.started = None;
            self.playing = false;
        }
        latest
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.stop();
    }
}

pub fn fmt_time(t: f64) -> String {
    let t = t.max(0.0) as u64;
    if t >= 3600 {
        format!("{}:{:02}:{:02}", t / 3600, t / 60 % 60, t % 60)
    } else {
        format!("{}:{:02}", t / 60, t % 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Half a second of a quiet tone as a WAV file.
    fn wav(path: &Path) {
        let rate = 8000u32;
        let samples: Vec<i16> = (0..rate / 2).map(|i| ((i as f32 * 0.3).sin() * 8000.0) as i16).collect();
        let data = (samples.len() * 2) as u32;
        let mut b = Vec::new();
        b.extend_from_slice(b"RIFF");
        b.extend_from_slice(&(36 + data).to_le_bytes());
        b.extend_from_slice(b"WAVEfmt ");
        b.extend_from_slice(&16u32.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes()); // PCM
        b.extend_from_slice(&1u16.to_le_bytes()); // mono
        b.extend_from_slice(&rate.to_le_bytes());
        b.extend_from_slice(&(rate * 2).to_le_bytes());
        b.extend_from_slice(&2u16.to_le_bytes());
        b.extend_from_slice(&16u16.to_le_bytes());
        b.extend_from_slice(b"data");
        b.extend_from_slice(&data.to_le_bytes());
        for s in samples {
            b.extend_from_slice(&s.to_le_bytes());
        }
        std::fs::write(path, b).unwrap();
    }

    /// Music is decoded in-process: its length is known, and the samples come
    /// out as the -1..1 floats the sound output wants.
    #[test]
    fn music_decodes() {
        let path = std::env::temp_dir().join(format!("duopane-media-{}.wav", std::process::id()));
        wav(&path);
        let dec = rodio::Decoder::new(BufReader::new(File::open(&path).unwrap())).unwrap();
        let secs = dec.total_duration().unwrap().as_secs_f64();
        assert!((secs - 0.5).abs() < 0.01, "{secs}");
        let all: Vec<rodio::Sample> = dec.collect();
        assert_eq!(all.len(), 4000);
        assert!(all.iter().all(|s| (-1.0..=1.0).contains(s)) && all.iter().any(|s| s.abs() > 0.1));
        let _ = std::fs::remove_file(&path);
    }
}
