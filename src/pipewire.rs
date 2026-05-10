use std::{
    ffi::OsString,
    path::PathBuf,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Clone)]
pub struct AudioStream {
    pub id: u32,
    pub app_name: Option<String>,
    pub process_binary: Option<String>,
    pub node_name: Option<String>,
    pub description: Option<String>,
}

impl AudioStream {
    pub fn display_name(&self) -> &str {
        self.app_name
            .as_deref()
            .or(self.process_binary.as_deref())
            .or(self.node_name.as_deref())
            .unwrap_or("unknown")
    }

    fn matches(&self, needle: &str) -> bool {
        let needle = needle.to_lowercase();
        [
            self.app_name.as_deref(),
            self.process_binary.as_deref(),
            self.node_name.as_deref(),
            self.description.as_deref(),
        ]
        .into_iter()
        .flatten()
        .any(|value| value.to_lowercase().contains(&needle))
    }
}

#[derive(Debug)]
pub struct CaptureOptions {
    pub target_id: u32,
    pub output: PathBuf,
    pub seconds: Option<u64>,
    pub rate: u32,
    pub channels: u8,
}

#[derive(Debug, Deserialize)]
struct PwObject {
    id: u32,
    #[serde(rename = "type")]
    object_type: String,
    info: Option<PwInfo>,
}

#[derive(Debug, Deserialize)]
struct PwInfo {
    props: Option<PwProps>,
}

#[derive(Debug, Deserialize)]
struct PwProps {
    #[serde(flatten)]
    values: serde_json::Map<String, Value>,
}

pub fn list_audio_streams() -> Result<Vec<AudioStream>> {
    let output = Command::new("pw-dump")
        .output()
        .context("failed to run pw-dump; is PipeWire installed and running?")?;

    if !output.status.success() {
        bail!(
            "pw-dump failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    let objects: Vec<PwObject> =
        serde_json::from_slice(&output.stdout).context("failed to parse pw-dump JSON")?;

    let mut streams = objects
        .into_iter()
        .filter_map(audio_stream_from_object)
        .collect::<Vec<_>>();

    streams.sort_by_key(|stream| (stream.display_name().to_lowercase(), stream.id));
    Ok(streams)
}

pub fn wait_for_audio_stream(match_terms: &[String], timeout_seconds: u64) -> Result<AudioStream> {
    let deadline = Instant::now() + Duration::from_secs(timeout_seconds);

    loop {
        match find_audio_stream_by_terms(match_terms) {
            Ok(stream) => return Ok(stream),
            Err(error) if Instant::now() >= deadline => return Err(error),
            Err(_) => thread::sleep(Duration::from_millis(250)),
        }
    }
}

fn find_audio_stream_by_terms(match_terms: &[String]) -> Result<AudioStream> {
    let matches = matching_audio_streams(match_terms)?;

    match matches.as_slice() {
        [] => Err(anyhow!(
            "no active PipeWire playback stream matched {}. Start playback in the selected app and try again.",
            format_match_terms(match_terms)
        )),
        [stream] => Ok(stream.clone()),
        streams => {
            let choices = streams
                .iter()
                .map(|stream| format!("{} ({})", stream.display_name(), stream.id))
                .collect::<Vec<_>>()
                .join(", ");
            Err(anyhow!(
                "multiple streams matched {}: {choices}. Use a more specific app name.",
                format_match_terms(match_terms)
            ))
        }
    }
}

fn matching_audio_streams(match_terms: &[String]) -> Result<Vec<AudioStream>> {
    Ok(list_audio_streams()?
        .into_iter()
        .filter(|stream| match_terms.iter().any(|term| stream.matches(term)))
        .collect())
}

pub fn capture_stream(options: CaptureOptions) -> Result<()> {
    let mut args = vec![
        OsString::from("--record"),
        OsString::from("--target"),
        OsString::from(options.target_id.to_string()),
        OsString::from("--rate"),
        OsString::from(options.rate.to_string()),
        OsString::from("--channels"),
        OsString::from(options.channels.to_string()),
    ];

    if let Some(seconds) = options.seconds {
        let samples = u64::from(options.rate) * seconds;
        args.push(OsString::from("--sample-count"));
        args.push(OsString::from(samples.to_string()));
    }

    args.push(options.output.into_os_string());

    let status = Command::new("pw-cat")
        .args(args)
        .stdin(Stdio::null())
        .status()
        .context("failed to run pw-cat; install PipeWire utilities")?;

    if !status.success() {
        bail!("pw-cat exited with status {status}");
    }

    Ok(())
}

fn audio_stream_from_object(object: PwObject) -> Option<AudioStream> {
    if !object.object_type.ends_with(":Node") {
        return None;
    }

    let props = object.info?.props?.values;
    if prop(&props, "media.class") != Some("Stream/Output/Audio") {
        return None;
    }

    Some(AudioStream {
        id: object.id,
        app_name: prop(&props, "application.name").map(str::to_owned),
        process_binary: prop(&props, "application.process.binary").map(str::to_owned),
        node_name: prop(&props, "node.name").map(str::to_owned),
        description: prop(&props, "node.description")
            .or_else(|| prop(&props, "media.name"))
            .map(str::to_owned),
    })
}

fn prop<'a>(props: &'a serde_json::Map<String, Value>, key: &str) -> Option<&'a str> {
    props.get(key)?.as_str()
}

fn format_match_terms(match_terms: &[String]) -> String {
    match_terms
        .iter()
        .map(|term| format!("'{term}'"))
        .collect::<Vec<_>>()
        .join(", ")
}
