use std::{
    env, fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use clap::{Args as ClapArgs, Subcommand};

pub mod codex;

const MODEL_NAMES: &[&str] = &[
    "gpt-5.5",
    "gpt-5.4",
    "gpt-5.4-mini",
    "gpt-5.3-codex",
    "gpt-5.2",
];
const REASONING_LEVELS: &[&str] = &["low", "medium", "high", "xhigh"];

pub struct SummaryRequest<'a> {
    pub transcript: &'a str,
    pub instruction: Option<&'a str>,
    pub model: Option<&'a str>,
    pub reasoning: Option<&'a str>,
}

pub trait LanguageModelProvider {
    fn authenticate(&self) -> Result<()>;
    fn auth_status(&self) -> Result<String>;
    fn ensure_authenticated(&self) -> Result<()>;
    fn summarize(&self, request: SummaryRequest<'_>) -> Result<String>;
}

#[derive(Debug, Default)]
pub struct Settings {
    pub model: Option<String>,
    pub reasoning: Option<String>,
}

impl Settings {
    pub fn model_display(&self) -> &str {
        self.model.as_deref().unwrap_or("provider default")
    }

    pub fn reasoning_display(&self) -> &str {
        self.reasoning.as_deref().unwrap_or("provider default")
    }
}

#[derive(Debug, ClapArgs)]
pub struct Args {
    /// List known model names for AI summaries.
    #[arg(long)]
    model_list: bool,

    /// Save the model name used by future AI summaries.
    #[arg(long, value_name = "NAME")]
    set_model: Option<String>,

    /// List supported reasoning levels.
    #[arg(long)]
    reasoning_list: bool,

    /// Save the reasoning level used by future AI summaries.
    #[arg(long, value_name = "LEVEL")]
    set_reasoning: Option<String>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Log in with ChatGPT through Codex and save credentials for future AI commands.
    Auth,
    /// Show whether the AI provider is authenticated.
    Status,
    /// Summarize a transcript using the configured AI provider.
    Summarize {
        /// Transcript text file.
        #[arg(short, long)]
        input: PathBuf,

        /// Extra instruction for the summary, such as audience or format.
        #[arg(short = 'n', long)]
        instruction: Option<String>,

        /// Model to use for this summary.
        #[arg(long)]
        model: Option<String>,

        /// Reasoning level to use for this summary.
        #[arg(long)]
        reasoning: Option<String>,
    },
}

pub fn run(args: Args) -> Result<()> {
    let provider = codex::CodexProvider;
    let mut handled = false;

    if args.model_list {
        print_model_list();
        handled = true;
    }

    if args.reasoning_list {
        print_reasoning_list();
        handled = true;
    }

    if let Some(model) = args.set_model {
        set_model(&model)?;
        println!("Saved AI model: {model}");
        handled = true;
    }

    if let Some(reasoning) = args.set_reasoning {
        set_reasoning(&reasoning)?;
        println!("Saved AI reasoning level: {reasoning}");
        handled = true;
    }

    match args.command {
        Some(Command::Auth) => {
            provider.authenticate()?;
            handled = true;
        }
        Some(Command::Status) => {
            let status = provider.auth_status()?;
            println!("{status}");
            if let Some(settings) = load_settings()? {
                println!("Model: {}", settings.model_display());
                println!("Reasoning: {}", settings.reasoning_display());
            }
            handled = true;
        }
        Some(Command::Summarize {
            input,
            instruction,
            model,
            reasoning,
        }) => {
            provider.ensure_authenticated()?;
            let transcript = read_transcript(&input)?;
            let settings = load_settings()?.unwrap_or_default();
            let summary = provider.summarize(SummaryRequest {
                transcript: &transcript,
                instruction: instruction.as_deref(),
                model: model.as_deref().or(settings.model.as_deref()),
                reasoning: reasoning.as_deref().or(settings.reasoning.as_deref()),
            })?;
            println!("{summary}");
            handled = true;
        }
        None => {}
    }

    if !handled {
        bail!("no AI action requested; try `palantwire ai --help`");
    }

    Ok(())
}

fn print_model_list() {
    for model in MODEL_NAMES {
        println!("{model}");
    }
}

fn print_reasoning_list() {
    for level in REASONING_LEVELS {
        println!("{level}");
    }
}

fn set_model(model: &str) -> Result<()> {
    validate_name(model, "model")?;
    let mut settings = load_settings()?.unwrap_or_default();
    settings.model = Some(model.to_owned());
    save_settings(&settings)
}

fn set_reasoning(reasoning: &str) -> Result<()> {
    if !REASONING_LEVELS.contains(&reasoning) {
        bail!("unsupported reasoning level '{reasoning}'. Run `palantwire ai --reasoning-list`.");
    }

    let mut settings = load_settings()?.unwrap_or_default();
    settings.reasoning = Some(reasoning.to_owned());
    save_settings(&settings)
}

fn load_settings() -> Result<Option<Settings>> {
    let path = settings_path()?;
    if !path.exists() {
        return Ok(None);
    }

    let contents =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let mut settings = Settings::default();

    for line in contents.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        if value.is_empty() {
            continue;
        }

        match key.trim() {
            "model" => settings.model = Some(value.to_owned()),
            "reasoning" => settings.reasoning = Some(value.to_owned()),
            _ => {}
        }
    }

    Ok(Some(settings))
}

fn save_settings(settings: &Settings) -> Result<()> {
    let path = settings_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    let mut contents = String::new();
    if let Some(model) = &settings.model {
        contents.push_str("model=");
        contents.push_str(model);
        contents.push('\n');
    }
    if let Some(reasoning) = &settings.reasoning {
        contents.push_str("reasoning=");
        contents.push_str(reasoning);
        contents.push('\n');
    }

    fs::write(&path, contents).with_context(|| format!("failed to write {}", path.display()))
}

fn settings_path() -> Result<PathBuf> {
    if let Some(config_home) = env::var_os("XDG_CONFIG_HOME") {
        return Ok(PathBuf::from(config_home).join("palantwire/ai.conf"));
    }

    let home = env::var_os("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home).join(".config/palantwire/ai.conf"))
}

fn validate_name(value: &str, kind: &str) -> Result<()> {
    if value.trim().is_empty() {
        bail!("{kind} cannot be empty");
    }
    if value
        .chars()
        .any(|ch| ch.is_whitespace() || matches!(ch, '=' | '"' | '\'' | '\\'))
    {
        bail!("{kind} contains unsupported characters");
    }
    Ok(())
}

fn read_transcript(input: &Path) -> Result<String> {
    let transcript = fs::read_to_string(input)
        .with_context(|| format!("failed to read transcript {}", input.display()))?;

    if transcript.trim().is_empty() {
        bail!("transcript is empty");
    }

    Ok(transcript)
}
