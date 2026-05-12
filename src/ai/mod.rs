use std::{env, fs, path::PathBuf};

use anyhow::{Context, Result, bail};

pub mod codex;

pub const MODEL_NAMES: &[&str] = &[
    "gpt-5.5",
    "gpt-5.4",
    "gpt-5.4-mini",
    "gpt-5.3-codex",
    "gpt-5.2",
];
const REASONING_LEVELS: &[&str] = &["low", "medium", "high", "xhigh"];

pub fn reasoning_levels() -> &'static [&'static str] {
    REASONING_LEVELS
}

pub struct SummaryRequest<'a> {
    pub transcript: &'a str,
    pub prior_summary: Option<&'a str>,
    pub instruction: Option<&'a str>,
    pub model: Option<&'a str>,
    pub reasoning: Option<&'a str>,
}

pub trait LanguageModelProvider {
    fn authenticate(&self) -> Result<()>;
    fn ensure_authenticated(&self) -> Result<()>;
    fn summarize(&self, request: SummaryRequest<'_>) -> Result<String>;
}

#[derive(Debug, Default)]
pub struct Settings {
    pub model: Option<String>,
    pub reasoning: Option<String>,
}

pub fn set_model(model: &str) -> Result<()> {
    validate_name(model, "model")?;
    let mut settings = load_settings()?.unwrap_or_default();
    settings.model = Some(model.to_owned());
    save_settings(&settings)
}

pub fn set_reasoning(reasoning: &str) -> Result<()> {
    if !REASONING_LEVELS.contains(&reasoning) {
        bail!("unsupported reasoning level '{reasoning}'.");
    }

    let mut settings = load_settings()?.unwrap_or_default();
    settings.reasoning = Some(reasoning.to_owned());
    save_settings(&settings)
}

pub fn load_settings() -> Result<Option<Settings>> {
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
