use std::{
    collections::HashMap,
    ffi::OsString,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Clone)]
pub struct AudioStream {
    pub id: u32,
    pub serial: u32,
    pub app_name: Option<String>,
    pub process_id: Option<u32>,
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
pub struct StreamSelector {
    pub process_id: Option<u32>,
    pub match_terms: Vec<String>,
}

#[derive(Debug)]
pub struct RawCaptureOptions {
    pub target_id: u32,
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
    let client_process_ids = client_process_ids(&objects);

    let mut streams = objects
        .into_iter()
        .filter_map(|object| audio_stream_from_object(object, &client_process_ids))
        .collect::<Vec<_>>();

    streams.sort_by_key(|stream| (stream.display_name().to_lowercase(), stream.id));
    Ok(streams)
}

pub fn wait_for_audio_stream(
    selector: &StreamSelector,
    timeout_seconds: u64,
) -> Result<AudioStream> {
    let deadline = Instant::now() + Duration::from_secs(timeout_seconds);

    loop {
        match find_audio_stream(selector) {
            Ok(stream) => return Ok(stream),
            Err(error) if Instant::now() >= deadline => return Err(error),
            Err(_) => thread::sleep(Duration::from_millis(250)),
        }
    }
}

fn find_audio_stream(selector: &StreamSelector) -> Result<AudioStream> {
    let streams = list_audio_streams()?;

    if let Some(process_id) = selector.process_id {
        let pid_matches = streams
            .iter()
            .filter(|stream| stream.process_id == Some(process_id))
            .cloned()
            .collect::<Vec<_>>();

        if let Some(stream) = single_stream_match(&pid_matches, "PID", &process_id.to_string())? {
            return Ok(stream);
        }
    }

    let matches = streams
        .into_iter()
        .filter(|stream| selector.match_terms.iter().any(|term| stream.matches(term)))
        .collect::<Vec<_>>();

    match matches.as_slice() {
        [] => Err(anyhow!(
            "no active PipeWire playback stream matched {}. Start playback in the selected app and try again.",
            format_selector(selector)
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
                format_selector(selector)
            ))
        }
    }
}

fn single_stream_match(
    streams: &[AudioStream],
    kind: &str,
    value: &str,
) -> Result<Option<AudioStream>> {
    match streams {
        [] => Ok(None),
        [stream] => Ok(Some(stream.clone())),
        streams => {
            let choices = streams
                .iter()
                .map(|stream| format!("{} ({})", stream.display_name(), stream.id))
                .collect::<Vec<_>>()
                .join(", ");
            Err(anyhow!(
                "multiple streams matched {kind} {value}: {choices}. Pick a more specific stream."
            ))
        }
    }
}

pub fn spawn_raw_capture(options: RawCaptureOptions) -> Result<Child> {
    let mut args = vec![
        OsString::from("--record"),
        OsString::from("--target"),
        OsString::from(options.target_id.to_string()),
        OsString::from("--rate"),
        OsString::from(options.rate.to_string()),
        OsString::from("--channels"),
        OsString::from(options.channels.to_string()),
        OsString::from("--format"),
        OsString::from("s16"),
        OsString::from("--raw"),
    ];

    if let Some(seconds) = options.seconds {
        let samples = u64::from(options.rate) * seconds;
        args.push(OsString::from("--sample-count"));
        args.push(OsString::from(samples.to_string()));
    }

    args.push(OsString::from("-"));

    Command::new("pw-cat")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
        .context("failed to run pw-cat; install PipeWire utilities")
}

fn audio_stream_from_object(
    object: PwObject,
    client_process_ids: &HashMap<u32, u32>,
) -> Option<AudioStream> {
    if !object.object_type.ends_with(":Node") {
        return None;
    }

    let props = object.info?.props?.values;
    if prop(&props, "media.class") != Some("Stream/Output/Audio") {
        return None;
    }

    Some(AudioStream {
        id: object.id,
        serial: prop_u32(&props, "object.serial").unwrap_or(object.id),
        app_name: prop(&props, "application.name").map(str::to_owned),
        process_id: process_id(&props, client_process_ids),
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

fn prop_u32(props: &serde_json::Map<String, Value>, key: &str) -> Option<u32> {
    props
        .get(key)
        .and_then(|value| value.as_u64().and_then(|value| u32::try_from(value).ok()))
        .or_else(|| prop(props, key).and_then(|value| value.parse().ok()))
}

fn process_id(
    props: &serde_json::Map<String, Value>,
    client_process_ids: &HashMap<u32, u32>,
) -> Option<u32> {
    prop_u32(props, "application.process.id").or_else(|| {
        prop_u32(props, "client.id")
            .and_then(|client_id| client_process_ids.get(&client_id).copied())
    })
}

fn client_process_ids(objects: &[PwObject]) -> HashMap<u32, u32> {
    objects
        .iter()
        .filter(|object| object.object_type.ends_with(":Client"))
        .filter_map(|object| {
            let props = &object.info.as_ref()?.props.as_ref()?.values;
            let process_id = prop_u32(props, "application.process.id")?;
            Some((object.id, process_id))
        })
        .collect()
}

fn format_selector(selector: &StreamSelector) -> String {
    let mut parts = selector
        .match_terms
        .iter()
        .map(|term| format!("'{term}'"))
        .collect::<Vec<_>>();

    if let Some(process_id) = selector.process_id {
        parts.insert(0, format!("PID {process_id}"));
    }

    parts.join(", ")
}
