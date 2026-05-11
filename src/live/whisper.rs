use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};

use crate::audio::recording::WavWriter;

pub struct WhisperPreview {
    whisper_cli: PathBuf,
    model: PathBuf,
    sample_rate: u32,
    chunk_bytes: usize,
    label_transcript: bool,
    temp_dir: PathBuf,
    pending: Vec<u8>,
    chunk_index: u64,
    previous_ended_sentence: Option<bool>,
}

pub trait PcmSink {
    fn write_pcm(&mut self, pcm: &[u8]) -> Result<()>;
}

impl WhisperPreview {
    pub fn validate_dependencies(model: &Path) -> Result<()> {
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
        _verbose: bool,
        label_transcript: bool,
    ) -> Result<Self> {
        Self::validate_dependencies(model)?;
        let chunk_bytes = chunk_size(sample_rate, chunk_seconds)?;
        let temp_dir = create_temp_dir()?;

        Ok(Self {
            whisper_cli: whisper_cli(),
            model: model.to_owned(),
            sample_rate,
            chunk_bytes,
            label_transcript,
            temp_dir,
            pending: Vec::with_capacity(chunk_bytes),
            chunk_index: 0,
            previous_ended_sentence: None,
        })
    }

    pub fn finish(mut self) -> Result<()> {
        if !self.pending.is_empty() {
            self.transcribe_pending_chunk()?;
        }

        if self.previous_ended_sentence.is_some() && !self.label_transcript {
            println!();
        }

        Ok(())
    }

    fn write_pcm_inner(&mut self, pcm: &[u8]) -> Result<()> {
        self.pending.extend_from_slice(pcm);

        while self.pending.len() >= self.chunk_bytes {
            let chunk = self.pending.drain(..self.chunk_bytes).collect::<Vec<_>>();
            self.transcribe_chunk(&chunk)?;
        }

        Ok(())
    }

    fn transcribe_pending_chunk(&mut self) -> Result<()> {
        let chunk = std::mem::take(&mut self.pending);
        self.transcribe_chunk(&chunk)
    }

    fn transcribe_chunk(&mut self, pcm: &[u8]) -> Result<()> {
        let wav_path = self
            .temp_dir
            .join(format!("chunk-{:06}.wav", self.chunk_index));
        self.chunk_index += 1;

        let writer = WavWriter::create(&wav_path, self.sample_rate, 1)?;
        writer
            .write_all_pcm_and_finalize(pcm, self.sample_rate, 1)
            .with_context(|| format!("failed to write {}", wav_path.display()))?;

        let output = Command::new(&self.whisper_cli)
            .arg("--model")
            .arg(&self.model)
            .arg("--file")
            .arg(&wav_path)
            .args(["--language", "en", "--no-timestamps", "--no-prints"])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .with_context(|| format!("failed to run {}", self.whisper_cli.display()))?;

        if output.status.success() {
            let text = clean_output(&String::from_utf8_lossy(&output.stdout));
            if !text.is_empty() {
                self.emit_transcript(&text)?;
            }
        }

        let _ = fs::remove_file(wav_path);
        Ok(())
    }

    fn emit_transcript(&mut self, text: &str) -> Result<()> {
        if self.label_transcript {
            println!("whisper: {text}");
            self.previous_ended_sentence = Some(true);
            return Ok(());
        }

        let text = split_sentence_lines(text);
        match self.previous_ended_sentence {
            Some(true) if !text.starts_with('\n') => println!(),
            Some(false) => print!(" "),
            _ => {}
        }
        print!("{text}");
        io::stdout().flush().context("failed to flush transcript")?;

        self.previous_ended_sentence = Some(text.trim_end().ends_with('.'));
        Ok(())
    }
}

impl Drop for WhisperPreview {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.temp_dir);
    }
}

impl PcmSink for WhisperPreview {
    fn write_pcm(&mut self, pcm: &[u8]) -> Result<()> {
        self.write_pcm_inner(pcm)
    }
}

trait WavWriterExt {
    fn write_all_pcm_and_finalize(self, pcm: &[u8], sample_rate: u32, channels: u16) -> Result<()>;
}

impl WavWriterExt for WavWriter {
    fn write_all_pcm_and_finalize(
        mut self,
        pcm: &[u8],
        sample_rate: u32,
        channels: u16,
    ) -> Result<()> {
        self.write_pcm(pcm)?;
        self.finalize(sample_rate, channels)
    }
}

fn whisper_cli() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tools/whisper.cpp/build/bin/whisper-cli")
}

fn chunk_size(sample_rate: u32, chunk_seconds: u32) -> Result<usize> {
    (sample_rate as usize)
        .checked_mul(2)
        .and_then(|bytes| bytes.checked_mul(chunk_seconds as usize))
        .filter(|bytes| *bytes > 0)
        .context("Whisper chunk size is too large")
}

fn create_temp_dir() -> Result<PathBuf> {
    let pid = std::process::id();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before Unix epoch")?
        .as_nanos();
    let temp_dir = std::env::temp_dir().join(format!("palantwire-whisper-{pid}-{nanos}"));
    fs::create_dir(&temp_dir)
        .with_context(|| format!("failed to create {}", temp_dir.display()))?;
    Ok(temp_dir)
}

fn clean_output(output: &str) -> String {
    output
        .lines()
        .filter_map(|line| {
            let line = strip_leading_bracketed_timestamp(line).trim();
            (!line.is_empty()).then_some(line)
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn strip_leading_bracketed_timestamp(line: &str) -> &str {
    let line = line.trim_start();
    if !line.starts_with('[') {
        return line;
    }

    let Some(end) = line.find(']') else {
        return line;
    };

    line[end + 1..].trim_start()
}

fn split_sentence_lines(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();

    while let Some(ch) = chars.next() {
        output.push(ch);
        if ch == '.' {
            let mut consumed_whitespace = false;
            while matches!(chars.peek(), Some(next) if next.is_whitespace()) {
                consumed_whitespace = true;
                chars.next();
            }
            if consumed_whitespace && chars.peek().is_some() {
                output.push('\n');
            }
        }
    }

    output.trim().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_output_strips_whisper_timestamps() {
        assert_eq!(
            clean_output("  [00:00:00.000 --> 00:00:01.000] hello\nworld"),
            "hello world"
        );
    }

    #[test]
    fn sentence_split_matches_python_worker_behavior() {
        assert_eq!(
            split_sentence_lines("One. Two.  Three"),
            "One.\nTwo.\nThree"
        );
    }

    #[test]
    fn chunk_size_uses_mono_s16le_bytes() {
        assert_eq!(chunk_size(16_000, 5).unwrap(), 160_000);
    }
}
