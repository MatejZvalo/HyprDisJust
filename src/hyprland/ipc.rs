use std::env;
use std::io::{BufRead, BufReader, ErrorKind};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, Context};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MonitorSocketEvent {
    Added,
    Removed,
    AddedV2,
    RemovedV2,
}

impl MonitorSocketEvent {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Added => "monitoradded",
            Self::Removed => "monitorremoved",
            Self::AddedV2 => "monitoraddedv2",
            Self::RemovedV2 => "monitorremovedv2",
        }
    }
}

pub struct Socket2EventReader {
    reader: BufReader<UnixStream>,
}

impl Socket2EventReader {
    pub fn connect_from_env() -> anyhow::Result<Self> {
        let path = socket2_path_from_env()?;
        Self::connect(path)
    }

    pub fn connect(path: PathBuf) -> anyhow::Result<Self> {
        let stream = UnixStream::connect(&path)
            .with_context(|| format!("failed to connect to {}", path.display()))?;
        Ok(Self {
            reader: BufReader::new(stream),
        })
    }

    pub fn read_monitor_event(&mut self) -> anyhow::Result<Option<MonitorSocketEvent>> {
        self.read_monitor_event_with_timeout(None)
    }

    pub fn read_monitor_event_timeout(
        &mut self,
        timeout: Duration,
    ) -> anyhow::Result<Option<MonitorSocketEvent>> {
        self.read_monitor_event_with_timeout(Some(timeout))
    }

    fn read_monitor_event_with_timeout(
        &mut self,
        timeout: Option<Duration>,
    ) -> anyhow::Result<Option<MonitorSocketEvent>> {
        self.reader
            .get_ref()
            .set_read_timeout(timeout)
            .context("failed to set Hyprland socket2 read timeout")?;

        loop {
            let mut line = String::new();
            match self.reader.read_line(&mut line) {
                Ok(0) => return Err(anyhow!("Hyprland socket2 closed")),
                Ok(_) => {
                    if let Some(event) = parse_monitor_event(&line) {
                        return Ok(Some(event));
                    }
                }
                Err(error)
                    if timeout.is_some()
                        && matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) =>
                {
                    return Ok(None);
                }
                Err(error) => {
                    return Err(error).context("failed to read Hyprland socket2 event");
                }
            }
        }
    }
}

pub fn socket2_path_from_env() -> anyhow::Result<PathBuf> {
    let runtime_dir = env::var_os("XDG_RUNTIME_DIR")
        .ok_or_else(|| anyhow!("XDG_RUNTIME_DIR is not set; cannot locate Hyprland socket2"))?;
    let signature = env::var_os("HYPRLAND_INSTANCE_SIGNATURE").ok_or_else(|| {
        anyhow!("HYPRLAND_INSTANCE_SIGNATURE is not set; cannot locate Hyprland socket2")
    })?;

    if runtime_dir.as_os_str().is_empty() {
        return Err(anyhow!(
            "XDG_RUNTIME_DIR is empty; cannot locate Hyprland socket2"
        ));
    }
    if signature.as_os_str().is_empty() {
        return Err(anyhow!(
            "HYPRLAND_INSTANCE_SIGNATURE is empty; cannot locate Hyprland socket2"
        ));
    }

    Ok(socket2_path(runtime_dir, signature))
}

pub fn socket2_path(runtime_dir: impl Into<PathBuf>, signature: impl AsRef<Path>) -> PathBuf {
    runtime_dir
        .into()
        .join("hypr")
        .join(signature)
        .join(".socket2.sock")
}

pub fn parse_monitor_event(line: &str) -> Option<MonitorSocketEvent> {
    let event_name = line.trim_end_matches(['\r', '\n']).split_once(">>")?.0;
    match event_name {
        "monitoradded" => Some(MonitorSocketEvent::Added),
        "monitorremoved" => Some(MonitorSocketEvent::Removed),
        "monitoraddedv2" => Some(MonitorSocketEvent::AddedV2),
        "monitorremovedv2" => Some(MonitorSocketEvent::RemovedV2),
        _ => None,
    }
}
