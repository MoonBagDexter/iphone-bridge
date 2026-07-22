//! Dictation: buffer the phone's mic PCM, hand it to whisper.cpp, get text back.
//!
//! The phone streams f32 stereo-interleaved PCM at 48kHz (see mic-worklet.js --
//! mono sources are duplicated across both channels). whisper.cpp wants 16kHz
//! mono, and its bundled decoder handles WAV but not the AAC an iPhone would
//! otherwise produce, so we do the conversion ourselves rather than shelling
//! out to ffmpeg.

use anyhow::{bail, Context, Result};
use arboard::Clipboard;
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

pub const TARGET_RATE: u32 = 16_000;

/// Encode mono f32 samples as a 16-bit PCM WAV at 16kHz.
pub fn encode_wav_16k_mono(samples: &[f32]) -> Vec<u8> {
    const HEADER: usize = 44;
    let data_len = samples.len() * 2;
    let mut w = Vec::with_capacity(HEADER + data_len);

    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&((36 + data_len) as u32).to_le_bytes());
    w.extend_from_slice(b"WAVE");
    w.extend_from_slice(b"fmt ");
    w.extend_from_slice(&16u32.to_le_bytes()); // PCM chunk size
    w.extend_from_slice(&1u16.to_le_bytes()); // format: PCM
    w.extend_from_slice(&1u16.to_le_bytes()); // channels: mono
    w.extend_from_slice(&TARGET_RATE.to_le_bytes());
    w.extend_from_slice(&(TARGET_RATE * 2).to_le_bytes()); // byte rate
    w.extend_from_slice(&2u16.to_le_bytes()); // block align
    w.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    w.extend_from_slice(b"data");
    w.extend_from_slice(&(data_len as u32).to_le_bytes());

    for &s in samples {
        // Scale by 32768 then clamp, so -1.0 reaches i16::MIN instead of
        // stopping a bit short and leaving the waveform lopsided.
        let v = (s * 32_768.0)
            .round()
            .clamp(i16::MIN as f32, i16::MAX as f32) as i16;
        w.extend_from_slice(&v.to_le_bytes());
    }
    w
}

/// Non-speech annotations whisper emits inside parentheses. Bracketed spans
/// are always markers, but parentheses also appear in real speech, so only
/// these exact contents are dropped.
const PAREN_MARKERS: [&str; 3] = ["silence", "blank_audio", "inaudible"];

/// Strip whisper.cpp's non-speech annotations and normalise whitespace.
///
/// Silence yields markers like `[BLANK_AUDIO]` or `(silence)`; typing those
/// into the focused field would be worse than typing nothing.
pub fn clean_transcript(raw: &str) -> String {
    let mut kept = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();

    while let Some(c) = chars.next() {
        match c {
            '[' => {
                for skipped in chars.by_ref() {
                    if skipped == ']' {
                        break;
                    }
                }
                kept.push(' ');
            }
            '(' => {
                let mut inner = String::new();
                let mut closed = false;
                for n in chars.by_ref() {
                    if n == ')' {
                        closed = true;
                        break;
                    }
                    inner.push(n);
                }
                let is_marker = PAREN_MARKERS.contains(&inner.trim().to_lowercase().as_str());
                if is_marker {
                    kept.push(' ');
                } else {
                    // Unmatched or ordinary parenthetical -- put it back verbatim.
                    kept.push('(');
                    kept.push_str(&inner);
                    if closed {
                        kept.push(')');
                    }
                }
            }
            _ => kept.push(c),
        }
    }

    kept.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Where whisper.cpp lives. Sits next to config.json under the app's data dir
/// so the bridge owns it rather than depending on a system-wide install.
pub fn whisper_dir() -> PathBuf {
    dirs_local_appdata()
        .join("iphone-bridge")
        .join("whisper")
}

fn dirs_local_appdata() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
}

pub fn whisper_exe() -> PathBuf {
    whisper_dir().join("whisper-cli.exe")
}

pub fn whisper_model() -> PathBuf {
    whisper_dir().join("ggml-large-v3-turbo-q5_0.bin")
}

/// Locate ffmpeg on PATH.
///
/// Needed only for the Shortcut upload path: whisper.cpp's bundled decoder
/// reads WAV/MP3/FLAC but not the AAC an iPhone records.
pub fn find_ffmpeg() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join("ffmpeg.exe"))
        .find(|candidate| candidate.is_file())
}

/// Convert an arbitrary recording into the 16kHz mono WAV whisper wants.
///
/// ffmpeg reads the container from the data itself, so the phone can send
/// m4a, mp3, wav or anything else without us having to sniff it.
pub fn convert_to_wav_16k(input: &[u8]) -> Result<Vec<u8>> {
    let ffmpeg = find_ffmpeg().context(
        "ffmpeg not found on PATH -- needed to decode recordings uploaded from the phone",
    )?;

    let seq = SCRATCH_SEQ.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let src = std::env::temp_dir().join(format!("iphone-bridge-upload-{pid}-{seq}"));
    let dst = std::env::temp_dir().join(format!("iphone-bridge-upload-{pid}-{seq}.wav"));
    std::fs::write(&src, input).with_context(|| format!("writing {}", src.display()))?;

    let run = Command::new(&ffmpeg)
        .args(["-y", "-loglevel", "error", "-i"])
        .arg(&src)
        .args([
            "-ac", "1",
            "-ar", &TARGET_RATE.to_string(),
            "-c:a", "pcm_s16le",
            "-f", "wav",
        ])
        .arg(&dst)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .with_context(|| format!("spawning {}", ffmpeg.display()));

    let _ = std::fs::remove_file(&src);
    let out = run?;

    if !out.status.success() {
        let _ = std::fs::remove_file(&dst);
        bail!(
            "could not decode the uploaded audio: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }

    let wav = std::fs::read(&dst).with_context(|| format!("reading {}", dst.display()))?;
    let _ = std::fs::remove_file(&dst);
    Ok(wav)
}

/// Transcribe a recording uploaded from the phone. Does NOT deliver -- the
/// caller decides whether the text should be typed anywhere.
pub fn transcribe_upload(input: &[u8]) -> Result<String> {
    let wav = convert_to_wav_16k(input)?;
    transcribe_wav(&wav)
}

/// Put the transcript where the user expects it: on the clipboard, then typed
/// into whatever has focus.
///
/// Clipboard first -- if focus moves mid-type the text is still recoverable,
/// whereas the reverse order can leave nothing behind.
pub fn deliver(text: &str) -> Result<()> {
    if text.is_empty() {
        return Ok(());
    }
    match Clipboard::new().and_then(|mut c| c.set_text(text.to_string())) {
        Ok(_) => {}
        Err(e) => eprintln!("[dictate] clipboard failed: {e}"),
    }
    crate::keyboard::type_text(text);
    Ok(())
}

/// Everything after the audio stops: transcribe, run the active mode over the
/// transcript, then deliver whatever that produced.
///
/// The `plan` is made by the caller *before* transcription because it decides the
/// vocabulary prompt whisper needs as an input.
pub fn finish_and_deliver(
    mono_16k: &[f32],
    cfg: &crate::voice::settings::VoiceSettings,
    plan: &crate::voice::Plan,
) -> Result<crate::voice::Processed> {
    if mono_16k.is_empty() {
        return Ok(crate::voice::Processed {
            raw: String::new(),
            text: String::new(),
            warning: None,
        });
    }
    let seconds = mono_16k.len() as f32 / TARGET_RATE as f32;
    let wav = encode_wav_16k_mono(mono_16k);
    let raw = transcribe_wav_with_prompt(&wav, plan.whisper_prompt.as_deref())?;
    let processed = crate::voice::apply(&raw, cfg, plan, seconds);
    deliver(&processed.text)?;
    Ok(processed)
}

/// Ten minutes of 16kHz mono. Long enough that nobody hits it mid-thought,
/// short enough that a phone left transmitting can't eat the machine.
pub const MAX_SAMPLES: usize = TARGET_RATE as usize * 60 * 10;

/// Accumulates the phone's mic stream, downsampling as it arrives.
///
/// Converting on the fly rather than buffering raw 48kHz stereo cuts memory
/// sixfold, which is what makes a ten-minute cap affordable. Frames that don't
/// divide evenly into a resample group carry over to the next chunk, so the
/// result is identical however the stream happens to be split across
/// WebSocket messages -- and iOS splits it unpredictably.
#[derive(Default)]
pub struct DictationBuffer {
    active: bool,
    /// Mono frames not yet forming a whole resample group.
    carry: Vec<f32>,
    /// A left channel whose right half landed in the next chunk.
    half_frame: Option<f32>,
    mono_16k: Vec<f32>,
    overflowed: bool,
}

/// 48kHz in, 16kHz out.
const DECIMATION: usize = 3;

impl DictationBuffer {
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Begin a session, discarding anything left from the last one.
    pub fn start(&mut self) {
        self.active = true;
        self.carry.clear();
        self.half_frame = None;
        self.mono_16k.clear();
        self.overflowed = false;
    }

    /// Append a chunk of f32 stereo-interleaved 48kHz samples.
    pub fn push(&mut self, interleaved: &[f32]) {
        if !self.active {
            return;
        }

        // Finish the frame the previous chunk cut in half.
        let mut rest = interleaved;
        if let Some(left) = self.half_frame.take() {
            match rest.split_first() {
                Some((right, tail)) => {
                    self.carry.push((left + right) / 2.0);
                    rest = tail;
                }
                None => {
                    self.half_frame = Some(left);
                    return;
                }
            }
        }

        let mut frames = rest.chunks_exact(2);
        for f in frames.by_ref() {
            self.carry.push((f[0] + f[1]) / 2.0);
        }
        if let Some(&left) = frames.remainder().first() {
            self.half_frame = Some(left);
        }

        let groups = self.carry.len() / DECIMATION;
        for g in 0..groups {
            if self.mono_16k.len() >= MAX_SAMPLES {
                self.overflowed = true;
                break;
            }
            let sum: f32 = self.carry[g * DECIMATION..(g + 1) * DECIMATION].iter().sum();
            self.mono_16k.push(sum / DECIMATION as f32);
        }
        self.carry.drain(..groups * DECIMATION);
    }

    /// End the session and take the accumulated 16kHz mono audio.
    pub fn finish(&mut self) -> Vec<f32> {
        self.active = false;
        self.carry.clear();
        self.half_frame = None;
        std::mem::take(&mut self.mono_16k)
    }

    /// True if the cap was hit and audio was dropped.
    pub fn overflowed(&self) -> bool {
        self.overflowed
    }
}

/// Decode little-endian f32 PCM as it arrives off the WebSocket.
pub fn decode_f32_le(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

/// Stops a console window flashing up on every dictation -- the bridge itself
/// runs windowless, and whisper-cli is a console app.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

static SCRATCH_SEQ: AtomicU64 = AtomicU64::new(0);

/// Run whisper.cpp over a WAV and return the cleaned transcript.
pub fn transcribe_wav(wav: &[u8]) -> Result<String> {
    transcribe_wav_with_prompt(wav, None)
}

/// As `transcribe_wav`, but biases recognition toward `prompt` -- a comma-separated
/// list of names, acronyms and jargon from the user's vocabulary. whisper.cpp takes
/// this as its initial decoder context, which is the documented mechanism for making
/// it favour spellings it would otherwise mangle.
pub fn transcribe_wav_with_prompt(wav: &[u8], prompt: Option<&str>) -> Result<String> {
    let exe = whisper_exe();
    let model = whisper_model();
    if !exe.exists() {
        bail!("whisper-cli.exe not found at {}", exe.display());
    }
    if !model.exists() {
        bail!("whisper model not found at {}", model.display());
    }

    let scratch = std::env::temp_dir().join(format!(
        "iphone-bridge-dictate-{}-{}.wav",
        std::process::id(),
        SCRATCH_SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::write(&scratch, wav)
        .with_context(|| format!("writing {}", scratch.display()))?;

    let mut cmd = Command::new(&exe);
    cmd.arg("-m")
        .arg(&model)
        .arg("-f")
        .arg(&scratch)
        .arg("--no-timestamps")
        .arg("--no-prints");
    if let Some(p) = prompt.map(str::trim).filter(|p| !p.is_empty()) {
        cmd.arg("--prompt").arg(p);
    }
    let result = cmd
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .with_context(|| format!("spawning {}", exe.display()));

    let _ = std::fs::remove_file(&scratch);
    let out = result?;

    if !out.status.success() {
        bail!(
            "whisper-cli exited with {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(clean_transcript(&String::from_utf8_lossy(&out.stdout)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Read a 16-bit PCM WAV into f32 samples, skipping the 44-byte header.
    fn decode_wav_i16(bytes: &[u8]) -> Vec<f32> {
        bytes[44..]
            .chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32_768.0)
            .collect()
    }

    /// A deterministic stereo stream: left ramps, right is its negation, so
    /// downmixing to mono must not simply pass one channel through.
    fn fake_stream(frames: usize) -> Vec<f32> {
        (0..frames)
            .flat_map(|i| {
                let v = i as f32 * 0.001;
                [v, -v * 0.5]
            })
            .collect()
    }

    #[test]
    fn decodes_f32_little_endian() {
        let mut bytes = Vec::new();
        for v in [0.0f32, 1.0, -0.5] {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        assert_eq!(decode_f32_le(&bytes), vec![0.0, 1.0, -0.5]);
    }

    #[test]
    fn decode_ignores_trailing_partial_sample() {
        let mut bytes = 1.0f32.to_le_bytes().to_vec();
        bytes.extend_from_slice(&[0xAA, 0xBB]); // half an f32
        assert_eq!(decode_f32_le(&bytes), vec![1.0]);
    }

    #[test]
    fn chunked_pushes_match_one_big_push() {
        // The core property: iOS splits the stream at arbitrary points, and
        // resample groups span 3 frames, so a naive implementation silently
        // loses or duplicates audio at every chunk boundary.
        let stream = fake_stream(600);

        let mut whole = DictationBuffer::default();
        whole.start();
        whole.push(&stream);
        let expected = whole.finish();

        let mut split = DictationBuffer::default();
        split.start();
        // Deliberately awkward sizes: none are multiples of 3 frames.
        let mut pos = 0;
        for chunk in [14usize, 2, 88, 7, 301, 4] {
            let take = chunk.min(stream.len() - pos);
            split.push(&stream[pos..pos + take]);
            pos += take;
            if pos >= stream.len() {
                break;
            }
        }
        split.push(&stream[pos..]);
        let actual = split.finish();

        assert_eq!(actual.len(), expected.len(), "chunking changed the length");
        for (i, (a, e)) in actual.iter().zip(expected.iter()).enumerate() {
            assert!((a - e).abs() < 1e-6, "sample {i}: {a} != {e}");
        }
    }

    #[test]
    fn start_discards_the_previous_session() {
        let mut b = DictationBuffer::default();
        b.start();
        b.push(&fake_stream(300));
        b.start();
        assert!(b.finish().is_empty(), "start must clear old audio");
    }

    #[test]
    fn push_is_ignored_when_not_started() {
        let mut b = DictationBuffer::default();
        b.push(&fake_stream(300));
        assert!(b.finish().is_empty());
    }

    #[test]
    fn finish_deactivates_and_empties() {
        let mut b = DictationBuffer::default();
        b.start();
        assert!(b.is_active());
        b.push(&fake_stream(300));
        assert!(!b.finish().is_empty());
        assert!(!b.is_active(), "finish must end the session");
        assert!(b.finish().is_empty(), "second finish yields nothing");
    }

    #[test]
    fn caps_runaway_recordings() {
        let mut b = DictationBuffer::default();
        b.start();
        // 3 input frames per output sample, so overshoot the cap comfortably.
        for _ in 0..40 {
            b.push(&fake_stream(MAX_SAMPLES / 10));
        }
        assert!(b.overflowed(), "should report dropping audio");
        assert!(b.finish().len() <= MAX_SAMPLES, "must not exceed the cap");
    }

    #[test]
    fn finds_ffmpeg_on_path() {
        // Not asserting presence -- only that lookup returns something usable
        // when it is installed, rather than a path that doesn't exist.
        if let Some(p) = find_ffmpeg() {
            assert!(p.exists(), "returned a path that isn't there: {p:?}");
        } else {
            eprintln!("SKIPPED: ffmpeg not on PATH");
        }
    }

    #[test]
    fn transcribes_an_uploaded_iphone_recording() {
        let sample = whisper_dir().join("jfk.wav");
        let ffmpeg = match find_ffmpeg() {
            Some(f) => f,
            None => {
                eprintln!("SKIPPED: ffmpeg not on PATH");
                return;
            }
        };
        if !whisper_exe().exists() || !whisper_model().exists() || !sample.exists() {
            eprintln!("SKIPPED: whisper.cpp not installed");
            return;
        }

        // Build an AAC/m4a the way an iPhone would record one -- the exact
        // format whisper.cpp refuses to decode on its own.
        let m4a_path = std::env::temp_dir().join("dictate-test-upload.m4a");
        let status = Command::new(&ffmpeg)
            .args(["-y", "-loglevel", "error", "-i"])
            .arg(&sample)
            .args(["-c:a", "aac", "-b:a", "64k", "-ar", "44100", "-ac", "1"])
            .arg(&m4a_path)
            .creation_flags(CREATE_NO_WINDOW)
            .status()
            .expect("run ffmpeg");
        assert!(status.success(), "ffmpeg failed to build the fixture");

        let m4a = std::fs::read(&m4a_path).expect("read m4a");
        let _ = std::fs::remove_file(&m4a_path);
        assert!(!m4a.is_empty());

        // Whisper must not get anything usable from the raw AAC, or the
        // conversion step is pointless. Note it exits 0 and returns an empty
        // transcript rather than failing, so this checks the text, not status.
        let raw = transcribe_wav(&m4a).unwrap_or_default();
        assert!(
            !raw.to_lowercase().contains("ask not"),
            "whisper decoded AAC directly; the conversion step is now pointless"
        );

        let text = transcribe_upload(&m4a).expect("upload transcription failed");
        assert!(
            text.to_lowercase().contains("ask not what your country"),
            "expected the JFK line, got: {text:?}"
        );
    }

    #[test]
    fn rejects_audio_it_cannot_decode() {
        if find_ffmpeg().is_none() {
            eprintln!("SKIPPED: ffmpeg not on PATH");
            return;
        }
        let err = transcribe_upload(b"this is not audio at all").unwrap_err();
        assert!(
            err.to_string().to_lowercase().contains("decode"),
            "error should name the problem, got: {err}"
        );
    }

    #[test]
    fn transcribes_real_speech_end_to_end() {
        let sample = whisper_dir().join("jfk.wav");
        if !whisper_exe().exists() || !whisper_model().exists() || !sample.exists() {
            eprintln!("SKIPPED: whisper.cpp not installed at {:?}", whisper_dir());
            return;
        }

        // jfk.wav is already 16kHz mono. Blow it up into the shape the phone
        // actually sends -- 48kHz stereo interleaved -- by repeating each
        // sample 3x across 2 channels. to_mono_16k must recover the original
        // exactly, so this exercises the real conversion path rather than a
        // hand-made fixture.
        let original = decode_wav_i16(&std::fs::read(&sample).expect("read jfk.wav"));
        let mut fake_phone_stream = Vec::with_capacity(original.len() * 6);
        for &s in &original {
            for _ in 0..3 {
                fake_phone_stream.push(s); // left
                fake_phone_stream.push(s); // right
            }
        }

        let mono = convert(&fake_phone_stream);
        assert_eq!(mono.len(), original.len(), "round trip must preserve length");

        let wav = encode_wav_16k_mono(&mono);
        let text = transcribe_wav(&wav).expect("transcription failed");

        let lower = text.to_lowercase();
        assert!(
            lower.contains("ask not what your country"),
            "expected the JFK line, got: {text:?}"
        );
    }

    /// Push one chunk through a fresh buffer and take the result.
    fn convert(interleaved: &[f32]) -> Vec<f32> {
        let mut b = DictationBuffer::default();
        b.start();
        b.push(interleaved);
        b.finish()
    }

    /// Duplicate mono values across both channels, as mic-worklet.js does.
    fn as_stereo(mono: &[f32]) -> Vec<f32> {
        mono.iter().flat_map(|&s| [s, s]).collect()
    }

    #[test]
    fn downmixes_stereo_to_mono() {
        // 6 frames at 48kHz -> 2 samples at 16kHz. Left = 1.0, right = 0.0,
        // so a correct downmix averages to 0.5 rather than passing a channel.
        let interleaved: Vec<f32> = (0..6).flat_map(|_| [1.0f32, 0.0f32]).collect();
        let out = convert(&interleaved);
        assert_eq!(out.len(), 2, "48kHz -> 16kHz decimates by 3");
        for s in out {
            assert!((s - 0.5).abs() < 1e-6, "expected 0.5, got {s}");
        }
    }

    #[test]
    fn resamples_by_averaging_not_dropping() {
        // Averaging 3 samples must land on the mean, not on whichever sample
        // happened to come first.
        let out = convert(&as_stereo(&[0.0, 0.3, 0.6, 0.9, 1.2, 1.5]));
        assert_eq!(out.len(), 2);
        assert!((out[0] - 0.3).abs() < 1e-6, "got {}", out[0]);
        assert!((out[1] - 1.2).abs() < 1e-6, "got {}", out[1]);
    }

    #[test]
    fn drops_trailing_partial_frame() {
        // 7 frames = 2 whole groups of 3 with 1 left over.
        assert_eq!(convert(&as_stereo(&[0.0; 7])).len(), 2);
    }

    #[test]
    fn wav_header_is_well_formed() {
        let wav = encode_wav_16k_mono(&[0.0, 0.5, -0.5]);
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(&wav[12..16], b"fmt ");
        assert_eq!(u16::from_le_bytes([wav[22], wav[23]]), 1, "mono");
        assert_eq!(
            u32::from_le_bytes([wav[24], wav[25], wav[26], wav[27]]),
            16_000,
            "16kHz"
        );
        assert_eq!(u16::from_le_bytes([wav[34], wav[35]]), 16, "16-bit");
        assert_eq!(&wav[36..40], b"data");
        assert_eq!(wav.len(), 44 + 3 * 2, "44-byte header + i16 per sample");
    }

    #[test]
    fn wav_clamps_out_of_range_samples() {
        // f32 PCM can exceed +/-1.0; naive scaling would wrap and click.
        let wav = encode_wav_16k_mono(&[2.0, -2.0]);
        assert_eq!(i16::from_le_bytes([wav[44], wav[45]]), i16::MAX);
        assert_eq!(i16::from_le_bytes([wav[46], wav[47]]), i16::MIN);
    }

    #[test]
    fn strips_blank_audio_marker() {
        assert_eq!(clean_transcript(" [BLANK_AUDIO]\n"), "");
        assert_eq!(clean_transcript("(silence)"), "");
    }

    #[test]
    fn keeps_real_speech_and_trims() {
        assert_eq!(
            clean_transcript("  Hello there, this is a test.\n"),
            "Hello there, this is a test."
        );
    }

    #[test]
    fn collapses_whitespace_across_segment_joins() {
        // whisper emits one line per segment; joined naively they carry
        // leading spaces and newlines into the middle of the text.
        assert_eq!(
            clean_transcript(" First segment.\n  Second segment.\n"),
            "First segment. Second segment."
        );
    }

    #[test]
    fn strips_markers_but_keeps_surrounding_speech() {
        assert_eq!(
            clean_transcript("Hello [BLANK_AUDIO] world"),
            "Hello world"
        );
    }
}
