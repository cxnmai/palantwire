use std::{
    env, fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use clap::Subcommand;

#[derive(Debug, Default)]
pub struct AppConfig {
    pub whisper_model: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
pub enum WhisperCommand {
    /// Save the Whisper model path used by future capture commands.
    SetModel {
        /// Path to a whisper.cpp ggml model file.
        path: PathBuf,
    },
    /// Show the saved Whisper model path.
    Status,
}

pub fn run_whisper_command(command: WhisperCommand) -> Result<()> {
    match command {
        WhisperCommand::SetModel { path } => {
            set_whisper_model(&path)?;
            println!("Saved Whisper model: {}", path.display());
        }
        WhisperCommand::Status => {
            let config = load()?;
            if let Some(path) = config.whisper_model {
                println!("Whisper model: {}", path.display());
            } else {
                println!("Whisper model: not set");
            }
        }
    }

    Ok(())
}

pub fn load() -> Result<AppConfig> {
    let path = config_path()?;
    if !path.exists() {
        return Ok(AppConfig::default());
    }

    let contents =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let mut config = AppConfig::default();

    for line in contents.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        if value.is_empty() {
            continue;
        }

        if key.trim() == "whisper_model" {
            config.whisper_model = Some(PathBuf::from(value));
        }
    }

    Ok(config)
}

fn set_whisper_model(path: &Path) -> Result<()> {
    if !path.exists() {
        bail!("Whisper model does not exist: {}", path.display());
    }

    let mut config = load()?;
    config.whisper_model = Some(path.to_owned());
    save(&config)
}

fn save(config: &AppConfig) -> Result<()> {
    let path = config_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    let mut contents = String::new();
    if let Some(whisper_model) = &config.whisper_model {
        contents.push_str("whisper_model=");
        contents.push_str(&whisper_model.display().to_string());
        contents.push('\n');
    }

    fs::write(&path, contents).with_context(|| format!("failed to write {}", path.display()))
}

fn config_path() -> Result<PathBuf> {
    if let Some(config_home) = env::var_os("XDG_CONFIG_HOME") {
        return Ok(PathBuf::from(config_home).join("palantwire/config.conf"));
    }

    let home = env::var_os("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home).join(".config/palantwire/config.conf"))
}
