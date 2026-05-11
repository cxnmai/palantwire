use std::{
    io::Write,
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
};

use anyhow::{Context, Result, bail};

pub struct WhisperPreview {
    child: Child,
    stdin: ChildStdin,
}

pub trait PcmSink {
    fn write_pcm(&mut self, pcm: &[u8]) -> Result<()>;
}

impl WhisperPreview {
    pub fn validate_dependencies(model: &Path) -> Result<()> {
        let script = whisper_script();
        if !script.exists() {
            bail!("missing Whisper live preview worker: {}", script.display());
        }

        let whisper_cli = whisper_cli();
        if !whisper_cli.exists() {
            bail!("missing whisper-cli: {}", whisper_cli.display());
        }

        if !model.exists() {
            bail!("missing Whisper model: {}", model.display());
        }

        Ok(())
    }

    pub fn spawn(
        model: &Path,
        sample_rate: u32,
        chunk_seconds: u32,
        verbose: bool,
        label_transcript: bool,
    ) -> Result<Self> {
        Self::validate_dependencies(model)?;
        let script = whisper_script();
        let whisper_cli = whisper_cli();

        let mut command = Command::new("python3");
        command
            .arg(script)
            .arg("--model")
            .arg(model)
            .arg("--whisper-cli")
            .arg(whisper_cli)
            .arg("--rate")
            .arg(sample_rate.to_string())
            .arg("--chunk-seconds")
            .arg(chunk_seconds.to_string());

        if label_transcript {
            command.arg("--label");
        }

        let stderr = if verbose {
            Stdio::inherit()
        } else {
            Stdio::null()
        };

        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::inherit())
            .stderr(stderr)
            .spawn()
            .context("failed to start Whisper live preview worker")?;

        let stdin = child
            .stdin
            .take()
            .context("failed to open Whisper preview stdin")?;

        Ok(Self { child, stdin })
    }

    pub fn finish(mut self) -> Result<()> {
        drop(self.stdin);
        let status = self
            .child
            .wait()
            .context("failed to wait for Whisper preview worker")?;

        if !status.success() {
            bail!("Whisper preview worker exited with status {status}");
        }

        Ok(())
    }
}

fn whisper_script() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/whisper_live.py")
}

fn whisper_cli() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tools/whisper.cpp/build/bin/whisper-cli")
}

impl PcmSink for WhisperPreview {
    fn write_pcm(&mut self, pcm: &[u8]) -> Result<()> {
        self.stdin
            .write_all(pcm)
            .context("failed to send audio to Whisper preview")
    }
}
