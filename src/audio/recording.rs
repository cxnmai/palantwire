use std::{
    fs::File,
    io::{Seek, SeekFrom, Write},
    path::Path,
};

use anyhow::{Context, Result};

pub struct WavWriter {
    file: File,
    data_bytes: u32,
}

impl WavWriter {
    pub fn create(path: &Path, sample_rate: u32, channels: u16) -> Result<Self> {
        let mut file =
            File::create(path).with_context(|| format!("failed to create {}", path.display()))?;
        write_header(&mut file, sample_rate, channels, 0)?;

        Ok(Self {
            file,
            data_bytes: 0,
        })
    }

    pub fn write_pcm(&mut self, pcm: &[u8]) -> Result<()> {
        self.file
            .write_all(pcm)
            .context("failed to write WAV audio data")?;
        self.data_bytes = self.data_bytes.saturating_add(pcm.len() as u32);
        Ok(())
    }

    pub fn finalize(mut self, sample_rate: u32, channels: u16) -> Result<()> {
        self.file
            .seek(SeekFrom::Start(0))
            .context("failed to seek WAV header")?;
        write_header(&mut self.file, sample_rate, channels, self.data_bytes)?;
        self.file.flush().context("failed to flush WAV recording")
    }
}

fn write_header(file: &mut File, sample_rate: u32, channels: u16, data_bytes: u32) -> Result<()> {
    let bits_per_sample = 16u16;
    let block_align = channels * bits_per_sample / 8;
    let byte_rate = sample_rate * u32::from(block_align);
    let riff_size = 36u32.saturating_add(data_bytes);

    file.write_all(b"RIFF")?;
    file.write_all(&riff_size.to_le_bytes())?;
    file.write_all(b"WAVE")?;
    file.write_all(b"fmt ")?;
    file.write_all(&16u32.to_le_bytes())?;
    file.write_all(&1u16.to_le_bytes())?;
    file.write_all(&channels.to_le_bytes())?;
    file.write_all(&sample_rate.to_le_bytes())?;
    file.write_all(&byte_rate.to_le_bytes())?;
    file.write_all(&block_align.to_le_bytes())?;
    file.write_all(&bits_per_sample.to_le_bytes())?;
    file.write_all(b"data")?;
    file.write_all(&data_bytes.to_le_bytes())?;
    Ok(())
}
