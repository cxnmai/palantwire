use std::{
    io::{BufReader, Read},
    path::PathBuf,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};

use crate::{
    audio::recording::WavWriter,
    live::vosk::VoskPreview,
    pipewire::{self, AudioStream},
};

pub struct CaptureSession {
    pub stream: AudioStream,
    pub output: PathBuf,
    pub seconds: Option<u64>,
    pub rate: u32,
    pub channels: u8,
    pub vosk_model: Option<PathBuf>,
}

pub fn run_capture_session(session: CaptureSession) -> Result<()> {
    eprintln!(
        "Starting raw PipeWire capture from node {} at {} Hz, {} channel(s)",
        session.stream.id, session.rate, session.channels
    );

    let mut capture = pipewire::spawn_raw_capture(pipewire::RawCaptureOptions {
        target_id: session.stream.serial,
        seconds: session.seconds,
        rate: session.rate,
        channels: session.channels,
    })?;

    let stdout = capture
        .stdout
        .take()
        .context("failed to read pw-cat audio stream")?;
    let mut audio = BufReader::new(stdout);
    let mut recorder =
        WavWriter::create(&session.output, session.rate, u16::from(session.channels))?;

    eprintln!("Writing full WAV recording to {}", session.output.display());

    let mut preview = session
        .vosk_model
        .as_deref()
        .map(|model| VoskPreview::spawn(model, session.rate))
        .transpose()?;

    if preview.is_some() {
        eprintln!("Live Vosk preview enabled");
    } else {
        eprintln!("Live Vosk preview disabled; pass --vosk-model to enable it");
    }

    let mut buffer = vec![0u8; 4096];
    let mut total_bytes = 0u64;
    let mut last_progress = Instant::now();

    loop {
        let read = audio
            .read(&mut buffer)
            .context("failed to read raw PipeWire audio")?;
        if read == 0 {
            break;
        }

        let chunk = &buffer[..read];
        total_bytes += read as u64;
        recorder.write_pcm(chunk)?;

        if let Some(preview) = &mut preview {
            preview.write_pcm(chunk)?;
        }

        if last_progress.elapsed() >= Duration::from_secs(2) {
            let bytes_per_second = u64::from(session.rate) * u64::from(session.channels) * 2;
            let captured_seconds = total_bytes / bytes_per_second;
            eprintln!("Captured {captured_seconds}s of audio");
            last_progress = Instant::now();
        }
    }

    let status = capture.wait().context("failed to wait for pw-cat")?;
    if !status.success() && total_bytes == 0 {
        bail!("pw-cat exited with status {status}");
    } else if !status.success() {
        eprintln!("pw-cat exited with status {status} after audio capture completed");
    }

    recorder.finalize(session.rate, u16::from(session.channels))?;
    eprintln!("Saved {}", session.output.display());

    if let Some(preview) = preview {
        preview.finish()?;
    }

    Ok(())
}
