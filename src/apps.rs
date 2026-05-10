use std::{
    fs,
    path::Path,
    process::{Command, Stdio},
};

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;

use crate::pipewire::StreamSelector;

#[derive(Debug, Clone)]
pub struct OpenWindow {
    pub id: u64,
    pub title: String,
    pub app_id: String,
    pub pid: Option<u32>,
    pub process_name: Option<String>,
}

impl OpenWindow {
    pub fn display_name(&self) -> &str {
        if self.title.is_empty() {
            &self.app_id
        } else {
            &self.title
        }
    }

    pub fn pipewire_selector(&self) -> StreamSelector {
        let mut terms = vec![self.app_id.clone(), self.title.clone()];

        if let Some(process_name) = &self.process_name {
            terms.push(process_name.clone());
        }

        terms.retain(|term| !term.is_empty());
        terms.sort();
        terms.dedup();

        StreamSelector {
            process_id: self.pid,
            match_terms: terms,
        }
    }

    fn matches(&self, needle: &str) -> bool {
        let needle = needle.to_lowercase();
        self.title.to_lowercase().contains(&needle)
            || self.app_id.to_lowercase().contains(&needle)
            || self.id.to_string() == needle
            || self.pid.is_some_and(|pid| pid.to_string() == needle)
            || self
                .process_name
                .as_deref()
                .is_some_and(|name| name.to_lowercase().contains(&needle))
    }
}

#[derive(Debug, Deserialize)]
struct NiriWindow {
    id: u64,
    title: Option<String>,
    app_id: Option<String>,
    pid: Option<u32>,
}

pub fn list_open_windows() -> Result<Vec<OpenWindow>> {
    if niri_available() {
        return list_niri_windows();
    }

    bail!("open-window listing is only implemented for Niri right now");
}

pub fn find_open_window(query: &str) -> Result<OpenWindow> {
    let matches = list_open_windows()?
        .into_iter()
        .filter(|window| window.matches(query))
        .collect::<Vec<_>>();

    match matches.as_slice() {
        [] => Err(anyhow!("no open window matched '{query}'")),
        [window] => Ok(window.clone()),
        windows => {
            let choices = windows
                .iter()
                .take(8)
                .map(|window| {
                    format!(
                        "{} [{} pid:{}]",
                        window.display_name(),
                        window.app_id,
                        window
                            .pid
                            .map(|pid| pid.to_string())
                            .unwrap_or_else(|| "-".to_owned())
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            Err(anyhow!(
                "multiple open windows matched '{query}': {choices}. Use a title, app id, window id, or PID."
            ))
        }
    }
}

fn niri_available() -> bool {
    Command::new("niri")
        .args(["msg", "--help"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn list_niri_windows() -> Result<Vec<OpenWindow>> {
    let output = Command::new("niri")
        .args(["msg", "-j", "windows"])
        .output()
        .context("failed to run `niri msg -j windows`")?;

    if !output.status.success() {
        bail!(
            "niri window listing failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    let mut windows = serde_json::from_slice::<Vec<NiriWindow>>(&output.stdout)
        .context("failed to parse Niri window JSON")?
        .into_iter()
        .filter_map(|window| {
            let app_id = window.app_id?;
            let process_name = window.pid.and_then(process_name);

            Some(OpenWindow {
                id: window.id,
                title: window.title.unwrap_or_default(),
                app_id,
                pid: window.pid,
                process_name,
            })
        })
        .collect::<Vec<_>>();

    windows.sort_by_key(|window| (window.app_id.to_lowercase(), window.id));
    Ok(windows)
}

fn process_name(pid: u32) -> Option<String> {
    let proc_dir = Path::new("/proc").join(pid.to_string());

    if let Ok(cmdline) = fs::read(proc_dir.join("cmdline")) {
        let command = cmdline.split(|byte| *byte == 0).next()?;
        let command = String::from_utf8_lossy(command);

        if let Some(name) = Path::new(command.as_ref())
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
        {
            return Some(name.to_owned());
        }
    }

    fs::read_to_string(proc_dir.join("comm"))
        .ok()
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
}
