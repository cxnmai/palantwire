use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};

mod apps;
mod pipewire;

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
    /// Capture audio from a PipeWire stream owned by a matching app.
    Capture {
        /// Case-insensitive substring matched against open window title, app id, PID, or process.
        #[arg(short, long)]
        app: String,

        /// Audio file to write. Extension controls pw-cat container when supported.
        #[arg(short, long)]
        output: PathBuf,

        /// Stop after this many seconds. Omit to record until interrupted.
        #[arg(short, long)]
        seconds: Option<u64>,

        /// Sample rate passed to pw-cat.
        #[arg(long, default_value_t = 16_000)]
        rate: u32,

        /// Channel count passed to pw-cat.
        #[arg(long, default_value_t = 1)]
        channels: u8,

        /// Seconds to wait for the selected app to create a PipeWire playback stream.
        #[arg(long, default_value_t = 30)]
        wait: u64,
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
                    "{:>5}  {:<24}  {}",
                    stream.id,
                    stream.display_name(),
                    stream.description.as_deref().unwrap_or("-")
                );
            }
        }
        CommandKind::Capture {
            app,
            output,
            seconds,
            rate,
            channels,
            wait,
        } => {
            let window = apps::find_open_window(&app)?;
            let stream = pipewire::wait_for_audio_stream(&window.pipewire_match_terms(), wait)?;
            eprintln!(
                "Capturing '{}' from PipeWire node {} into {}",
                stream.display_name(),
                stream.id,
                output.display()
            );

            pipewire::capture_stream(pipewire::CaptureOptions {
                target_id: stream.id,
                output,
                seconds,
                rate,
                channels,
            })?;
        }
    }

    Ok(())
}
