use std::{
    io::{self, Write},
    path::{Path, PathBuf},
    process::Command as ProcessCommand,
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};

use crate::ai::LanguageModelProvider;

mod ai;
mod apps;
mod audio;
mod config;
mod live;
mod pipewire;
mod session;

#[derive(Debug, Parser)]
#[command(version, about = "Wayland-native meeting transcription utility")]
struct Cli {
    /// Start the interactive capture flow and save the raw transcript beside the summary.
    #[arg(long)]
    save_transcript: bool,

    #[command(subcommand)]
    command: Option<CommandKind>,
}

#[derive(Debug, Subcommand)]
enum CommandKind {
    /// Authenticate or configure Codex-backed AI summaries.
    Codex {
        #[command(subcommand)]
        command: Option<CodexCommand>,

        /// Save the Codex model used for future summaries.
        #[arg(long)]
        model: Option<String>,

        /// Save the Codex reasoning level used for future summaries.
        #[arg(long)]
        reasoning: Option<String>,
    },
    /// Configure the whisper.cpp executable path.
    Whisper {
        /// Path to the whisper.cpp whisper-cli executable.
        #[arg(long)]
        path: PathBuf,
    },
    /// Configure where palantwire writes summary and transcript files.
    Output {
        /// Directory for generated output files.
        #[arg(long)]
        path: PathBuf,
    },
    /// Start the interactive capture flow.
    Start {
        /// Save the raw transcript beside the summary.
        #[arg(long)]
        save_transcript: bool,
    },
}

#[derive(Debug, Subcommand)]
enum CodexCommand {
    /// Log in with ChatGPT through Codex.
    Auth,
    /// List supported Codex model and reasoning options.
    List,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Some(CommandKind::Codex {
            command,
            model,
            reasoning,
        }) => run_codex(command, model, reasoning),
        Some(CommandKind::Whisper { path }) => {
            config::set_whisper_cli(&path)?;
            println!("Saved whisper-cli path: {}", path.display());
            Ok(())
        }
        Some(CommandKind::Output { path }) => {
            config::set_output_dir(&path)?;
            println!("Saved output path: {}", path.display());
            Ok(())
        }
        Some(CommandKind::Start { save_transcript }) => run_start(save_transcript),
        None => run_start(cli.save_transcript),
    }
}

fn run_codex(
    command: Option<CodexCommand>,
    model: Option<String>,
    reasoning: Option<String>,
) -> Result<()> {
    let had_action = command.is_some() || model.is_some() || reasoning.is_some();

    match command {
        Some(CodexCommand::Auth) => ai::codex::CodexProvider.authenticate()?,
        Some(CodexCommand::List) => list_codex_options(),
        None => {}
    }

    if let Some(model) = model {
        ai::set_model(&model)?;
        println!("Saved Codex model: {model}");
    }

    if let Some(reasoning) = reasoning {
        ai::set_reasoning(&reasoning)?;
        println!("Saved Codex reasoning: {reasoning}");
    }

    if !had_action {
        bail!(
            "no Codex action requested; use `palantwire codex auth` or `palantwire codex --model <name> --reasoning <level>`"
        );
    }

    Ok(())
}

fn list_codex_options() {
    println!("Models:");
    for model in ai::MODEL_NAMES {
        println!("  {model}");
    }

    println!("\nReasoning:");
    for level in ai::reasoning_levels() {
        println!("  {level}");
    }
}

fn run_start(save_transcript: bool) -> Result<()> {
    let app_config = config::load()?;
    let whisper_cli = match app_config.whisper_cli {
        Some(path) => path,
        None => prompt_for_whisper_cli()?,
    };
    let whisper_model = match app_config.whisper_model {
        Some(path) => path,
        None => prompt_for_whisper_model()?,
    };
    live::whisper::WhisperPreview::validate_dependencies(&whisper_cli, &whisper_model)?;

    let output_dir = match app_config.output_dir {
        Some(path) => path,
        None => prompt_for_output_dir()?,
    };
    let ai_settings = prompt_for_missing_codex_settings(ai::load_settings()?.unwrap_or_default())?;

    let window = prompt_for_window()?;
    let summary_path = prompt_for_summary_path(&output_dir)?;
    let raw_transcript_path = save_transcript.then(|| transcript_path_for(&summary_path));
    let selector = window.pipewire_selector();

    println!(
        "Waiting for PipeWire audio from '{}'. Start playback in that app.",
        window.display_name()
    );
    let stream = wait_for_selected_stream(&selector)?;
    println!("Starting capture. Press Ctrl-C to stop.");

    session::run_capture_session(session::CaptureSession {
        stream,
        input_selector: selector,
        output: None,
        seconds: None,
        rate: 16_000,
        channels: 1,
        whisper_cli: Some(whisper_cli),
        whisper_model: Some(whisper_model),
        transcript_options: Some(live::summary::TranscriptOptions {
            summary_path: Some(summary_path),
            raw_transcript_path,
            instruction: None,
            model: ai_settings.model,
            reasoning: ai_settings.reasoning,
            render_terminal: true,
        }),
        whisper_chunk_seconds: 5,
        verbose: false,
        progress: false,
        label_transcript: false,
    })
}

fn prompt_for_whisper_cli() -> Result<PathBuf> {
    loop {
        let answer = prompt("whisper-cli path: ")?;
        let path = PathBuf::from(answer.trim());
        match config::set_whisper_cli(&path) {
            Ok(()) => {
                println!("Saved whisper-cli path: {}", path.display());
                return Ok(path);
            }
            Err(error) => println!("{error}"),
        }
    }
}

fn prompt_for_whisper_model() -> Result<PathBuf> {
    loop {
        let answer = prompt("Whisper model path: ")?;
        let path = PathBuf::from(answer.trim());
        match config::set_whisper_model(&path) {
            Ok(()) => {
                println!("Saved Whisper model: {}", path.display());
                return Ok(path);
            }
            Err(error) => println!("{error}"),
        }
    }
}

fn prompt_for_output_dir() -> Result<PathBuf> {
    let default = std::env::current_dir().context("failed to read current directory")?;
    let label = format!("Output path (blank for {}): ", default.display());
    let answer = prompt(&label)?;
    let path = if answer.trim().is_empty() {
        default
    } else {
        PathBuf::from(answer.trim())
    };

    config::set_output_dir(&path)?;
    println!("Saved output path: {}", path.display());
    Ok(path)
}

fn prompt_for_missing_codex_settings(mut settings: ai::Settings) -> Result<ai::Settings> {
    if settings.model.is_none() {
        println!("Select Codex model:");
        settings.model = Some(prompt_for_choice("Model", ai::MODEL_NAMES)?);
        ai::set_model(settings.model.as_deref().unwrap())?;
    }

    if settings.reasoning.is_none() {
        println!("Select Codex reasoning:");
        settings.reasoning = Some(prompt_for_choice("Reasoning", ai::reasoning_levels())?);
        ai::set_reasoning(settings.reasoning.as_deref().unwrap())?;
    }

    Ok(settings)
}

fn prompt_for_window() -> Result<apps::OpenWindow> {
    let windows = apps::list_open_windows()?;
    if windows.is_empty() {
        bail!("No open Niri windows found.");
    }

    println!("Select the app to capture:");
    for (index, window) in windows.iter().enumerate() {
        println!(
            "{:>2}. {:<18} pid:{:<8} {}",
            index + 1,
            window.app_id,
            window
                .pid
                .map(|pid| pid.to_string())
                .unwrap_or_else(|| "-".to_owned()),
            window.display_name()
        );
    }

    loop {
        let answer = prompt("App number or search: ")?;
        if answer.trim().is_empty() {
            continue;
        }

        if let Ok(index) = answer.trim().parse::<usize>()
            && let Some(window) = windows.get(index.saturating_sub(1))
        {
            return Ok(window.clone());
        }

        match apps::find_open_window(answer.trim()) {
            Ok(window) => return Ok(window),
            Err(error) => println!("{error}"),
        }
    }
}

fn prompt_for_choice(label: &str, choices: &[&str]) -> Result<String> {
    for (index, choice) in choices.iter().enumerate() {
        println!("{:>2}. {choice}", index + 1);
    }

    loop {
        let answer = prompt(&format!("{label} number or name: "))?;
        let answer = answer.trim();
        if answer.is_empty() {
            continue;
        }

        if let Ok(index) = answer.parse::<usize>()
            && let Some(choice) = choices.get(index.saturating_sub(1))
        {
            return Ok((*choice).to_owned());
        }

        if let Some(choice) = choices
            .iter()
            .find(|choice| choice.eq_ignore_ascii_case(answer))
        {
            return Ok((*choice).to_owned());
        }

        println!("Unknown {label}: {answer}");
    }
}

fn prompt_for_summary_path(output_dir: &Path) -> Result<PathBuf> {
    let answer = prompt("Summary file name (blank for date/time): ")?;
    let name = if answer.trim().is_empty() {
        default_summary_name()
    } else {
        answer.trim().to_owned()
    };
    let mut path = PathBuf::from(name);

    if path.extension().is_none() {
        path.set_extension("md");
    }
    if path.is_relative() {
        path = output_dir.join(path);
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    Ok(path)
}

fn prompt(label: &str) -> Result<String> {
    print!("{label}");
    io::stdout().flush().context("failed to flush prompt")?;

    let mut answer = String::new();
    io::stdin()
        .read_line(&mut answer)
        .context("failed to read prompt response")?;
    Ok(answer.trim_end().to_owned())
}

fn wait_for_selected_stream(selector: &pipewire::StreamSelector) -> Result<pipewire::AudioStream> {
    loop {
        match pipewire::wait_for_audio_stream(selector, 1) {
            Ok(stream) => return Ok(stream),
            Err(_) => thread::sleep(Duration::from_secs(1)),
        }
    }
}

fn default_summary_name() -> String {
    if let Ok(output) = ProcessCommand::new("date")
        .arg("+%Y-%m-%d_%H-%M-%S")
        .output()
        && output.status.success()
    {
        let timestamp = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if !timestamp.is_empty() {
            return format!("{timestamp}.md");
        }
    }

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format!("{timestamp}.md")
}

fn transcript_path_for(summary_path: &Path) -> PathBuf {
    let stem = summary_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.is_empty())
        .unwrap_or("summary");
    summary_path.with_file_name(format!("{stem}.transcript.txt"))
}
