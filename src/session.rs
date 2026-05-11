use std::{
    io::{BufReader, Read},
    path::PathBuf,
    process::Child,
    sync::mpsc::{self, Receiver},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};

use crate::{
    audio::recording::WavWriter,
    live::whisper::{PcmSink, WhisperPreview},
    pipewire::{self, AudioStream, StreamSelector},
};

pub struct CaptureSession {
    pub stream: AudioStream,
    pub input_selector: StreamSelector,
    pub output: Option<PathBuf>,
    pub seconds: Option<u64>,
    pub rate: u32,
    pub channels: u8,
    pub whisper_model: Option<PathBuf>,
    pub whisper_chunk_seconds: u32,
    pub verbose: bool,
    pub progress: bool,
    pub label_transcript: bool,
}

pub fn run_capture_session(session: CaptureSession) -> Result<()> {
    if session.whisper_model.is_some() && session.channels != 1 {
        bail!("Whisper preview currently supports mono capture only; use --channels 1");
    }

    let whisper_chunk_bytes = session
        .whisper_model
        .is_some()
        .then(|| {
            whisper_chunk_bytes(
                session.rate,
                session.channels,
                session.whisper_chunk_seconds,
            )
        })
        .transpose()?;
    let mut recorder = session
        .output
        .as_deref()
        .map(|output| WavWriter::create(output, session.rate, u16::from(session.channels)))
        .transpose()?;
    let mut whisper_preview = session
        .whisper_model
        .as_deref()
        .map(|model| {
            WhisperPreview::spawn(
                model,
                session.rate,
                session.whisper_chunk_seconds,
                session.verbose,
                session.label_transcript,
            )
        })
        .transpose()?;

    if session.verbose {
        eprintln!(
            "Starting raw PipeWire capture from node {} at {} Hz, {} channel(s)",
            session.stream.id, session.rate, session.channels
        );
        if let Some(output) = &session.output {
            eprintln!("Writing full WAV recording to {}", output.display());
        } else {
            eprintln!("WAV recording disabled");
        }
        if whisper_preview.is_some() {
            eprintln!(
                "Live whisper.cpp preview enabled with {}s chunks",
                session.whisper_chunk_seconds
            );
        } else {
            eprintln!("Live preview disabled; pass --whisper-model to enable it");
        }
    }

    let mut capture = pipewire::spawn_raw_capture(pipewire::RawCaptureOptions {
        target_id: session.stream.serial,
        seconds: session.seconds,
        rate: session.rate,
        channels: session.channels,
    })?;
    let output_stdout = capture
        .stdout
        .take()
        .context("failed to read pw-cat audio stream")?;
    let output_rx = spawn_audio_reader(output_stdout);
    let mut mic_capture = None;
    let mut mic_rx = None;
    let mut app_accepting_input = false;
    let mut last_input_check = Instant::now() - Duration::from_secs(1);
    let mut mic_pending_bytes = 0usize;

    let mut total_bytes = 0u64;
    let mut last_progress = Instant::now();

    while let Ok(chunk) = output_rx.recv() {
        if last_input_check.elapsed() >= Duration::from_millis(500) {
            app_accepting_input =
                pipewire::input_stream_active(&session.input_selector).unwrap_or(false);
            last_input_check = Instant::now();

            if app_accepting_input && mic_capture.is_none() && whisper_preview.is_some() {
                let (child, rx) = spawn_mic_capture(session.rate, session.channels)?;
                mic_capture = Some(child);
                mic_rx = Some(rx);
            } else if !app_accepting_input {
                if let (Some(rx), Some(whisper_preview), Some(chunk_bytes)) =
                    (&mic_rx, &mut whisper_preview, whisper_chunk_bytes)
                {
                    drain_mic_chunks_to_whisper(rx, whisper_preview, &mut mic_pending_bytes)?;
                    flush_mic_segment_to_whisper(
                        whisper_preview,
                        chunk_bytes,
                        &mut mic_pending_bytes,
                    )?;
                }
                stop_child(&mut mic_capture);
                mic_rx = None;
            }
        }

        total_bytes += chunk.len() as u64;
        if let Some(recorder) = &mut recorder {
            recorder.write_pcm(&chunk)?;
        }

        if app_accepting_input
            && let (Some(rx), Some(whisper_preview)) = (&mic_rx, &mut whisper_preview)
        {
            drain_mic_chunks_to_whisper(rx, whisper_preview, &mut mic_pending_bytes)?;
        }

        if session.progress && last_progress.elapsed() >= Duration::from_secs(2) {
            let bytes_per_second = u64::from(session.rate) * u64::from(session.channels) * 2;
            let captured_seconds = total_bytes / bytes_per_second;
            eprintln!("Captured {captured_seconds}s of audio");
            last_progress = Instant::now();
        }
    }

    let status = capture.wait().context("failed to wait for pw-cat")?;
    if let (Some(rx), Some(whisper_preview), Some(chunk_bytes)) =
        (&mic_rx, &mut whisper_preview, whisper_chunk_bytes)
    {
        drain_mic_chunks_to_whisper(rx, whisper_preview, &mut mic_pending_bytes)?;
        flush_mic_segment_to_whisper(whisper_preview, chunk_bytes, &mut mic_pending_bytes)?;
    }
    stop_child(&mut mic_capture);

    if !status.success() && total_bytes == 0 {
        bail!("pw-cat exited with status {status}");
    } else if session.verbose && !status.success() {
        eprintln!("pw-cat exited with status {status} after audio capture completed");
    }

    if let Some(recorder) = recorder {
        recorder.finalize(session.rate, u16::from(session.channels))?;
        if session.verbose
            && let Some(output) = &session.output
        {
            eprintln!("Saved {}", output.display());
        }
    }

    if let Some(whisper_preview) = whisper_preview {
        whisper_preview.finish()?;
    }

    Ok(())
}

fn spawn_mic_capture(rate: u32, channels: u8) -> Result<(Child, Receiver<Vec<u8>>)> {
    let mut child = pipewire::spawn_default_raw_capture(rate, channels)?;
    let stdout = child
        .stdout
        .take()
        .context("failed to read microphone audio stream")?;
    Ok((child, spawn_audio_reader(stdout)))
}

fn stop_child(child: &mut Option<Child>) {
    if let Some(mut child) = child.take() {
        let _ = child.kill();
        let _ = child.wait();
    }
}

fn spawn_audio_reader<R>(reader: R) -> Receiver<Vec<u8>>
where
    R: Read + Send + 'static,
{
    let (tx, rx) = mpsc::channel();

    thread::spawn(move || {
        let mut reader = BufReader::new(reader);
        let mut buffer = vec![0u8; 4096];

        while let Ok(read) = reader.read(&mut buffer) {
            if read == 0 {
                break;
            }
            if tx.send(buffer[..read].to_vec()).is_err() {
                break;
            }
        }
    });

    rx
}

fn drain_mic_chunks_to_whisper(
    rx: &Receiver<Vec<u8>>,
    whisper_preview: &mut impl PcmSink,
    pending_bytes: &mut usize,
) -> Result<()> {
    while let Ok(chunk) = rx.try_recv() {
        *pending_bytes += chunk.len();
        whisper_preview.write_pcm(&chunk)?;
    }
    Ok(())
}

fn whisper_chunk_bytes(rate: u32, channels: u8, chunk_seconds: u32) -> Result<usize> {
    (rate as usize)
        .checked_mul(usize::from(channels))
        .and_then(|bytes| bytes.checked_mul(2))
        .and_then(|bytes| bytes.checked_mul(chunk_seconds as usize))
        .filter(|bytes| *bytes > 0)
        .context("Whisper chunk size is too large")
}

fn flush_mic_segment_to_whisper(
    whisper_preview: &mut impl PcmSink,
    chunk_bytes: usize,
    pending_bytes: &mut usize,
) -> Result<()> {
    let remainder = *pending_bytes % chunk_bytes;
    if remainder == 0 {
        return Ok(());
    }

    let padding = vec![0; chunk_bytes - remainder];
    whisper_preview.write_pcm(&padding)?;
    *pending_bytes = 0;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct TestSink {
        writes: Vec<Vec<u8>>,
    }

    impl PcmSink for TestSink {
        fn write_pcm(&mut self, pcm: &[u8]) -> Result<()> {
            self.writes.push(pcm.to_vec());
            Ok(())
        }
    }

    #[test]
    fn whisper_chunk_size_uses_s16le_bytes() {
        assert_eq!(whisper_chunk_bytes(16_000, 1, 5).unwrap(), 160_000);
    }

    #[test]
    fn whisper_chunk_size_rejects_overflow() {
        assert!(whisper_chunk_bytes(u32::MAX, u8::MAX, u32::MAX).is_err());
    }

    #[test]
    fn flush_pads_partial_mic_segment_to_chunk_boundary() {
        let mut sink = TestSink::default();
        let mut pending_bytes = 6;

        flush_mic_segment_to_whisper(&mut sink, 10, &mut pending_bytes).unwrap();

        assert_eq!(pending_bytes, 0);
        assert_eq!(sink.writes, vec![vec![0; 4]]);
    }

    #[test]
    fn flush_does_not_pad_aligned_mic_segment() {
        let mut sink = TestSink::default();
        let mut pending_bytes = 10;

        flush_mic_segment_to_whisper(&mut sink, 10, &mut pending_bytes).unwrap();

        assert_eq!(pending_bytes, 10);
        assert!(sink.writes.is_empty());
    }
}
