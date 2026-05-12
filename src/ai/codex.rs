use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

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

    fn ensure_authenticated(&self) -> Result<()> {
        let status = auth_status()?;
        if status.contains("Logged in using ChatGPT") {
            return Ok(());
        }

        bail!("Codex is not logged in with ChatGPT. Run `palantwire codex auth` first.")
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
        bail!("Codex login status failed with {}", output.status)
    } else {
        bail!("Codex login status failed: {stderr}")
    }
}

fn summarize(request: SummaryRequest<'_>) -> Result<String> {
    let prompt = summary_prompt_with_prior(
        request.transcript,
        request.prior_summary,
        request.instruction,
    );
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

fn summary_prompt_with_prior(
    transcript: &str,
    prior_summary: Option<&str>,
    instruction: Option<&str>,
) -> String {
    let instruction = instruction.unwrap_or(
        "Maintain a chronological, easy-to-scan live markdown summary. Write straight down in order, organized with useful section headings as topics shift. Use prose, short paragraphs, emphasis, inline code, and occasional bullets only when they genuinely improve readability. Avoid a rigid template like key points, decisions, and action items.",
    );
    let prior_summary = prior_summary
        .map(str::trim)
        .filter(|summary| !summary.is_empty())
        .unwrap_or("(none yet)");

    format!(
        "\
You are maintaining a live markdown summary for a transcript captured by palantwire.

Instruction:
{instruction}

Prior summary:
{prior_summary}

New transcript segment:
{transcript}

Create only the markdown update that should be appended after the prior summary. Preserve continuity across arbitrary transcript boundaries. Do not invent details.

Output contract:
- Return only new markdown for the current transcript segment.
- Do not include, restate, rewrite, or improve any prior summary text.
- Do not wrap the response in code fences or add commentary.
- If the new transcript adds no durable information, return exactly: <!-- no update -->

Style requirements for the new markdown update:
- Keep it chronological and organized with meaningful section headings.
- Prefer readable prose and short paragraphs over pure bullet lists. Use bullet lists when applicable though.
- Use markdown structure that renders well in terminal: headings, bold, italics, inline code, blockquotes, and occasional bullets where helpful.
- Do not force fixed sections like Key Points, Decisions, or Action Items unless the transcript naturally calls for them.
- Use super condensed language that still sounds natural. No complicated wording, and give ideas in few words.
- Do NOT answer as if you're telling about a meeting, rather speak generally. So if someone asks a question don't say someone asked a question, talk about the discussion itself that was brought out.
- Ignore unimportant information and don't include it. Don't bother to summarize every little detail or conversation - maintain a threshold for importance.
"
    )
}
