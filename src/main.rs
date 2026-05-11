use std::path::PathBuf;

use anyhow::{Result, bail};
use clap::{Parser, Subcommand, value_parser};

mod ai;
mod apps;
mod audio;
mod live;
mod pipewire;
mod session;

#[derive(Debug, Parser)]
#[command(version, about = "Wayland-native audio transcription utility")]
struct Cli {
    #[command(subcommand)]
    command: CommandKind,
}

#[derive(Debug, Subcommand)]
enum CommandKind {
    /// List open windows that can be selected before playback starts.
    ListApps,
    /// List active PipeWire audio streams that can be captured right now.
    ListStreams,
    /// Authenticate and use Codex-powered AI features.
    Ai {
        #[command(flatten)]
        args: ai::Args,
    },
    /// Capture audio from a PipeWire stream owned by a matching app.
    Capture {
        /// Case-insensitive substring matched against open window title, app id, PID, or process.
        #[arg(short, long)]
        app: String,

        /// Optional WAV file to write for the full recording.
        #[arg(short, long)]
        output: Option<PathBuf>,

        /// Stop after this many seconds. Omit to record until interrupted.
        #[arg(short, long, value_parser = value_parser!(u64).range(1..))]
        seconds: Option<u64>,

        /// Sample rate passed to pw-cat.
        #[arg(long, default_value_t = 16_000, value_parser = value_parser!(u32).range(1..))]
        rate: u32,

        /// Channel count passed to pw-cat.
        #[arg(long, default_value_t = 1, value_parser = value_parser!(u8).range(1..))]
        channels: u8,

        /// Seconds to wait for the selected app to create a PipeWire playback stream.
        #[arg(long, default_value_t = 30)]
        wait: u64,

        /// Enable experimental whisper.cpp chunked preview with the given ggml model.
        #[arg(long)]
        whisper_model: Option<PathBuf>,

        /// Seconds per whisper.cpp preview chunk.
        #[arg(long, default_value_t = 5, value_parser = value_parser!(u32).range(1..))]
        whisper_chunk_seconds: u32,

        /// Print capture setup and completion diagnostics to stderr.
        #[arg(long)]
        verbose: bool,

        /// Print periodic capture progress to stderr.
        #[arg(long)]
        progress: bool,

        /// Prefix transcript chunks with "whisper:".
        #[arg(long)]
        label_transcript: bool,
    },
}

fn main() -> Result<()> {
    match Cli::parse().command {
        CommandKind::ListApps => {
            let windows = apps::list_open_windows()?;

            if windows.is_empty() {
                println!("No open windows found.");
                return Ok(());
            }

            for window in windows {
                println!(
                    "{:>5}  {:<24}  pid:{:<8}  {}",
                    window.id,
                    window.app_id,
                    window
                        .pid
                        .map(|pid| pid.to_string())
                        .unwrap_or_else(|| "-".to_owned()),
                    window.display_name()
                );
            }
        }
        CommandKind::ListStreams => {
            let streams = pipewire::list_audio_streams()?;

            if streams.is_empty() {
                println!("No active PipeWire playback streams found.");
                return Ok(());
            }

            for stream in streams {
                println!(
                    "{:>5}  serial:{:<6}  {:<24}  pid:{:<8}  {}",
                    stream.id,
                    stream.serial,
                    stream.display_name(),
                    stream
                        .process_id
                        .map(|pid| pid.to_string())
                        .unwrap_or_else(|| "-".to_owned()),
                    stream.description.as_deref().unwrap_or("-")
                );
            }
        }
        CommandKind::Ai { args } => ai::run(args)?,
        CommandKind::Capture {
            app,
            output,
            seconds,
            rate,
            channels,
            wait,
            whisper_model,
            whisper_chunk_seconds,
            verbose,
            progress,
            label_transcript,
        } => {
            if whisper_model.is_some() && channels != 1 {
                bail!("Whisper preview currently supports mono capture only; use --channels 1");
            }
            if let Some(model) = &whisper_model {
                live::whisper::WhisperPreview::validate_dependencies(model)?;
            }

            let window = apps::find_open_window(&app)?;
            let selector = window.pipewire_selector();
            let stream = pipewire::wait_for_audio_stream(&selector, wait)?;
            if verbose {
                eprintln!(
                    "Capturing '{}' from PipeWire node {} serial {}",
                    stream.display_name(),
                    stream.id,
                    stream.serial
                );
            }

            session::run_capture_session(session::CaptureSession {
                stream,
                input_selector: selector,
                output,
                seconds,
                rate,
                channels,
                whisper_model,
                whisper_chunk_seconds,
                verbose,
                progress,
                label_transcript,
            })?;
        }
    }

    Ok(())
}
