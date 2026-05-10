use std::{
    io::Write,
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
};

use anyhow::{Context, Result, bail};

pub struct VoskPreview {
    child: Child,
    stdin: ChildStdin,
}

impl VoskPreview {
    pub fn spawn(model: &Path, sample_rate: u32) -> Result<Self> {
        let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/vosk_live.py");

        let mut child = Command::new("python3")
            .arg(script)
            .arg("--model")
            .arg(model)
            .arg("--rate")
            .arg(sample_rate.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()
            .context("failed to start Vosk live preview worker")?;

        let stdin = child
            .stdin
            .take()
            .context("failed to open Vosk preview stdin")?;

        Ok(Self { child, stdin })
    }

    pub fn write_pcm(&mut self, pcm: &[u8]) -> Result<()> {
        self.stdin
            .write_all(pcm)
            .context("failed to send audio to Vosk preview")
    }

    pub fn finish(mut self) -> Result<()> {
        drop(self.stdin);
        let status = self
            .child
            .wait()
            .context("failed to wait for Vosk preview worker")?;

        if !status.success() {
            bail!("Vosk preview worker exited with status {status}");
        }

        Ok(())
    }
}
