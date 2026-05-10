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

impl WhisperPreview {
    pub fn spawn(model: &Path, sample_rate: u32, chunk_seconds: u32) -> Result<Self> {
        let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/whisper_live.py");
        let whisper_cli = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tools/whisper.cpp/build/bin/whisper-cli");

        let mut child = Command::new("python3")
            .arg(script)
            .arg("--model")
            .arg(model)
            .arg("--whisper-cli")
            .arg(whisper_cli)
            .arg("--rate")
            .arg(sample_rate.to_string())
            .arg("--chunk-seconds")
            .arg(chunk_seconds.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()
            .context("failed to start Whisper live preview worker")?;

        let stdin = child
            .stdin
            .take()
            .context("failed to open Whisper preview stdin")?;

        Ok(Self { child, stdin })
    }

    pub fn write_pcm(&mut self, pcm: &[u8]) -> Result<()> {
        self.stdin
            .write_all(pcm)
            .context("failed to send audio to Whisper preview")
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
