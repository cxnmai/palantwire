use std::process::{Command, Stdio};

use anyhow::{Context, Result, anyhow, bail};

use super::{LanguageModelProvider, SummaryRequest};

pub struct CodexProvider;

impl LanguageModelProvider for CodexProvider {
    fn authenticate(&self) -> Result<()> {
        let status = Command::new("codex")
            .args(["login", "--device-auth"])
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .context("failed to start `codex login --device-auth`; install Codex CLI first")?;

        if !status.success() {
            bail!("Codex login failed with status {status}");
        }

        self.ensure_authenticated()
    }

    fn auth_status(&self) -> Result<String> {
        auth_status()
    }

    fn ensure_authenticated(&self) -> Result<()> {
        let status = auth_status()?;
        if status.contains("Logged in using ChatGPT") {
            return Ok(());
        }

        bail!("Codex is not logged in with ChatGPT. Run `palantwire ai auth` first.")
    }

    fn summarize(&self, request: SummaryRequest<'_>) -> Result<String> {
        summarize(request)
    }
}

fn auth_status() -> Result<String> {
    let output = Command::new("codex")
        .args(["login", "status"])
        .output()
        .context("failed to run `codex login status`; install Codex CLI first")?;

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();

    if output.status.success() {
        let status = if stdout.is_empty() {
            stderr
                .lines()
                .find(|line| line.starts_with("Logged in"))
                .unwrap_or("")
                .to_owned()
        } else {
            stdout
        };

        if status.is_empty() {
            Ok("Codex login status is unavailable.".to_owned())
        } else {
            Ok(status)
        }
    } else if stderr.is_empty() {
        Err(anyhow!("Codex login status failed with {}", output.status))
    } else {
        Err(anyhow!("Codex login status failed: {stderr}"))
    }
}

fn summarize(request: SummaryRequest<'_>) -> Result<String> {
    let prompt = summary_prompt(request.transcript, request.instruction);
    let mut command = Command::new("codex");
    command.args([
        "exec",
        "--skip-git-repo-check",
        "--ephemeral",
        "--sandbox",
        "read-only",
    ]);

    if let Some(model) = request.model {
        command.args(["--model", model]);
    }
    if let Some(reasoning) = request.reasoning {
        command.args(["--config", &format!("model_reasoning_effort={reasoning:?}")]);
    }

    let output = command
        .arg(prompt)
        .output()
        .context("failed to run `codex exec` for summary")?;

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();

    if !output.status.success() {
        if stderr.is_empty() {
            bail!("Codex summary failed with {}", output.status);
        }
        bail!("Codex summary failed: {stderr}");
    }

    if stdout.is_empty() {
        bail!("Codex returned an empty summary");
    }

    Ok(stdout)
}

fn summary_prompt(transcript: &str, instruction: Option<&str>) -> String {
    let instruction = instruction.unwrap_or(
        "Create a concise, useful summary with key points, decisions, and action items when present.",
    );

    format!(
        "\
You are summarizing a transcript captured by palantwire.

Instruction:
{instruction}

Transcript:
{transcript}
"
    )
}
