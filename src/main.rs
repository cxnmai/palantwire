use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};

mod pipewire;

#[derive(Debug, Parser)]
#[command(version, about = "Wayland-native audio transcription utility")]
struct Cli {
    #[command(subcommand)]
    command: CommandKind,
}

#[derive(Debug, Subcommand)]
enum CommandKind {
    /// List active PipeWire audio streams that can be targeted.
    ListApps,
    /// Capture audio from a PipeWire stream owned by a matching app.
    Capture {
        /// Case-insensitive substring matched against app/process/node names.
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
    },
}

fn main() -> Result<()> {
    match Cli::parse().command {
        CommandKind::ListApps => {
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
        } => {
            let stream = pipewire::find_audio_stream(&app)?;
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
