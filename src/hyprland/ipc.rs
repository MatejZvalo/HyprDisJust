use std::env;
use std::ffi::OsStr;
use std::fs;
use std::io::{BufRead, BufReader, ErrorKind};
use std::os::unix::fs::FileTypeExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

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
        let deadline = timeout.map(|timeout| Instant::now() + timeout);

        loop {
            if let Some(deadline) = deadline {
                let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                    return Ok(None);
                };
                self.reader
                    .get_ref()
                    .set_read_timeout(Some(remaining))
                    .context("failed to set Hyprland socket2 read timeout")?;
            } else {
                self.reader
                    .get_ref()
                    .set_read_timeout(None)
                    .context("failed to set Hyprland socket2 read timeout")?;
            }

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

    if runtime_dir.as_os_str().is_empty() {
        return Err(anyhow!(
            "XDG_RUNTIME_DIR is empty; cannot locate Hyprland socket2"
        ));
    }

    let signature = env::var_os("HYPRLAND_INSTANCE_SIGNATURE");
    resolve_socket2_path_with(&runtime_dir, signature.as_deref(), |path| {
        fs::metadata(path)
            .map(|metadata| metadata.file_type().is_socket())
            .unwrap_or(false)
    })
}

pub fn discover_socket2_path(runtime_dir: impl AsRef<Path>) -> anyhow::Result<PathBuf> {
    discover_socket2_path_with(runtime_dir, |path| {
        fs::metadata(path)
            .map(|metadata| metadata.file_type().is_socket())
            .unwrap_or(false)
    })
}

pub fn resolve_socket2_path_with(
    runtime_dir: impl AsRef<Path>,
    signature: Option<&OsStr>,
    is_socket: impl Fn(&Path) -> bool,
) -> anyhow::Result<PathBuf> {
    let runtime_dir = runtime_dir.as_ref();
    if let Some(signature) = signature.filter(|signature| !signature.is_empty()) {
        let configured = socket2_path(runtime_dir, signature);
        if is_socket(&configured) {
            return Ok(configured);
        }

        return discover_socket2_path_with(runtime_dir, &is_socket).with_context(|| {
            format!(
                "configured Hyprland socket2 {} is unavailable and no unique live replacement could be discovered",
                configured.display()
            )
        });
    }

    discover_socket2_path_with(runtime_dir, is_socket)
}

pub fn discover_socket2_path_with(
    runtime_dir: impl AsRef<Path>,
    is_socket: impl Fn(&Path) -> bool,
) -> anyhow::Result<PathBuf> {
    let hypr_dir = runtime_dir.as_ref().join("hypr");
    let entries = fs::read_dir(&hypr_dir)
        .with_context(|| format!("failed to read Hyprland runtime dir {}", hypr_dir.display()))?;
    let mut candidates = Vec::new();

    for entry in entries {
        let entry = entry.with_context(|| {
            format!(
                "failed to read an entry in Hyprland runtime dir {}",
                hypr_dir.display()
            )
        })?;
        let socket_path = entry.path().join(".socket2.sock");
        if is_socket(&socket_path) {
            candidates.push(socket_path);
        }
    }

    match candidates.len() {
        0 => Err(anyhow!(
            "HYPRLAND_INSTANCE_SIGNATURE is not set and no socket2 socket was found in {}",
            hypr_dir.display()
        )),
        1 => Ok(candidates.remove(0)),
        _ => {
            candidates.sort();
            Err(anyhow!(
                "HYPRLAND_INSTANCE_SIGNATURE is not set and multiple socket2 sockets were found: {}",
                candidates
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        }
    }
}

pub fn socket2_path(runtime_dir: impl Into<PathBuf>, signature: impl AsRef<Path>) -> PathBuf {
    runtime_dir
        .into()
        .join("hypr")
        .join(signature)
        .join(".socket2.sock")
}

pub fn parse_monitor_event(line: &str) -> Option<MonitorSocketEvent> {
    let (event_name, payload) = line.trim_end_matches(['\r', '\n']).split_once(">>")?;
    if payload.trim().is_empty() {
        return None;
    }
    match event_name {
        "monitoradded" => Some(MonitorSocketEvent::Added),
        "monitorremoved" => Some(MonitorSocketEvent::Removed),
        "monitoraddedv2" if valid_v2_monitor_payload(payload) => Some(MonitorSocketEvent::AddedV2),
        "monitorremovedv2" if valid_v2_monitor_payload(payload) => {
            Some(MonitorSocketEvent::RemovedV2)
        }
        _ => None,
    }
}

fn valid_v2_monitor_payload(payload: &str) -> bool {
    let mut fields = payload.split(',');
    fields
        .next()
        .is_some_and(|id| id.trim().parse::<i64>().is_ok())
        && fields.next().is_some_and(|name| !name.trim().is_empty())
}
