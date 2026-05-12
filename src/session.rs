use std::{
    collections::VecDeque,
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
    live::summary::{TranscriptOptions, TranscriptSinkBuilder},
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
    pub whisper_cli: Option<PathBuf>,
    pub whisper_model: Option<PathBuf>,
    pub transcript_options: Option<TranscriptOptions>,
    pub whisper_chunk_seconds: u32,
    pub verbose: bool,
    pub progress: bool,
    pub label_transcript: bool,
}

pub fn run_capture_session(session: CaptureSession) -> Result<()> {
    if session.whisper_model.is_some() && session.channels != 1 {
        bail!("Whisper preview currently supports mono capture only; use --channels 1");
    }
    if session.transcript_options.is_some() && session.whisper_model.is_none() {
        bail!("transcript output requires --whisper-model so transcript text can be generated");
    }
    if session.whisper_model.is_some() && session.whisper_cli.is_none() {
        bail!("Whisper capture requires a configured whisper-cli path");
    }

    let mut recorder = session
        .output
        .as_deref()
        .map(|output| WavWriter::create(output, session.rate, u16::from(session.channels)))
        .transpose()?;
    let mut whisper_preview = session
        .whisper_model
        .as_deref()
        .zip(session.whisper_cli.as_deref())
        .map(|(model, whisper_cli)| {
            WhisperPreview::spawn(
                whisper_cli,
                model,
                session.rate,
                session.whisper_chunk_seconds,
                session.verbose,
                session.label_transcript,
            )
        })
        .transpose()?;
    if let (Some(whisper_preview), Some(transcript_options)) =
        (&mut whisper_preview, session.transcript_options.clone())
    {
        whisper_preview
            .set_transcript_sink(TranscriptSinkBuilder::new(transcript_options).build()?);
    }

    let has_visible_or_saved_output = session.output.is_some()
        || session.whisper_model.is_some()
        || session.progress
        || session.verbose;

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
    } else if !has_visible_or_saved_output {
        eprintln!(
            "Capturing audio from '{}'. No output was requested; pass --output, --whisper-model, or --progress to see/save results. Press Ctrl-C to stop.",
            session.stream.display_name()
        );
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
    let mut mic_buffer = VecDeque::new();
    let mut app_accepting_input = false;
    let mut previous_app_accepting_input = false;
    let mut last_input_check = Instant::now() - Duration::from_secs(1);

    let mut total_bytes = 0u64;
    let mut last_progress = Instant::now();

    while let Ok(chunk) = output_rx.recv() {
        if last_input_check.elapsed() >= Duration::from_millis(500) {
            app_accepting_input =
                pipewire::input_stream_active(&session.input_selector).unwrap_or(false);
            last_input_check = Instant::now();

            if session.verbose && app_accepting_input != previous_app_accepting_input {
                if app_accepting_input {
                    eprintln!("Selected app is accepting input; mic transcription enabled");
                } else {
                    eprintln!("Selected app is not accepting input; mic transcription paused");
                }
                previous_app_accepting_input = app_accepting_input;
            }

            if app_accepting_input && mic_capture.is_none() && whisper_preview.is_some() {
                let (child, rx) = spawn_mic_capture(session.rate, session.channels)?;
                mic_capture = Some(child);
                mic_rx = Some(rx);
            } else if !app_accepting_input {
                stop_child(&mut mic_capture);
                mic_rx = None;
                mic_buffer.clear();
            }
        }

        total_bytes += chunk.len() as u64;
        if let Some(recorder) = &mut recorder {
            recorder.write_pcm(&chunk)?;
        }

        if let Some(whisper_preview) = &mut whisper_preview {
            let mut transcript_chunk = chunk.clone();
            if app_accepting_input {
                if let Some(rx) = &mic_rx {
                    drain_mic_chunks(rx, &mut mic_buffer);
                }
                mix_mic_into_output(&mut transcript_chunk, &mut mic_buffer);
            }
            whisper_preview.write_pcm(&transcript_chunk)?;
        }

        if session.progress && last_progress.elapsed() >= Duration::from_secs(2) {
            let bytes_per_second = u64::from(session.rate) * u64::from(session.channels) * 2;
            let captured_seconds = total_bytes / bytes_per_second;
            eprintln!("Captured {captured_seconds}s of audio");
            last_progress = Instant::now();
        }
    }

    let status = capture.wait().context("failed to wait for pw-cat")?;
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

fn drain_mic_chunks(rx: &Receiver<Vec<u8>>, buffer: &mut VecDeque<u8>) {
    while let Ok(chunk) = rx.try_recv() {
        buffer.extend(chunk);
    }
}

fn mix_mic_into_output(output: &mut [u8], mic: &mut VecDeque<u8>) {
    let pairs = output.len().min(mic.len()) / 2;

    for sample_index in 0..pairs {
        let byte_index = sample_index * 2;
        let app_sample = i16::from_le_bytes([output[byte_index], output[byte_index + 1]]);
        let mic_low = mic.pop_front().unwrap_or_default();
        let mic_high = mic.pop_front().unwrap_or_default();
        let mic_sample = i16::from_le_bytes([mic_low, mic_high]);
        let mixed = app_sample.saturating_add(mic_sample);
        output[byte_index..byte_index + 2].copy_from_slice(&mixed.to_le_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mix_mic_into_output_saturates_samples() {
        let mut output = 30_000i16.to_le_bytes().to_vec();
        let mut mic = VecDeque::from(10_000i16.to_le_bytes().to_vec());

        mix_mic_into_output(&mut output, &mut mic);

        assert_eq!(i16::from_le_bytes([output[0], output[1]]), i16::MAX);
        assert!(mic.is_empty());
    }
}
