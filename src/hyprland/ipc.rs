use std::env;
use std::ffi::OsStr;
use std::ffi::OsString;
use std::fs;
use std::io::{ErrorKind, Read};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::os::unix::net::UnixStream;
use std::path::{Component, Path, PathBuf};
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

const MAX_SOCKET_FRAME_BYTES: usize = 64 * 1024;

pub struct Socket2EventReader {
    stream: UnixStream,
    buffer: Vec<u8>,
}

impl Socket2EventReader {
    pub fn connect_from_env() -> anyhow::Result<Self> {
        let runtime_dir = runtime_dir_from_env()?;
        validate_runtime_ownership(&runtime_dir)?;
        validate_hypr_directory(&runtime_dir)?;
        let signature = env::var_os("HYPRLAND_INSTANCE_SIGNATURE");
        if let Some(signature) = signature.as_deref().filter(|value| !value.is_empty()) {
            validate_signature(signature)?;
            let configured = socket2_path(&runtime_dir, signature);
            validate_signature_directory(&runtime_dir, signature)?;
            if let Ok(reader) = Self::connect(configured.clone()) {
                return Ok(reader);
            }
        }
        let path = discover_socket2_path(&runtime_dir)?;
        Self::connect(path)
    }

    pub fn connect(path: PathBuf) -> anyhow::Result<Self> {
        validate_socket_inode(&path)?;
        let stream = UnixStream::connect(&path)
            .with_context(|| format!("failed to connect to {}", path.display()))?;
        validate_peer_credentials(&stream, &path)?;
        Ok(Self {
            stream,
            buffer: Vec::new(),
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
            if let Some(frame) = self.take_frame()? {
                if let Some(event) = parse_monitor_event(&frame) {
                    return Ok(Some(event));
                }
                continue;
            }
            if let Some(deadline) = deadline {
                let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                    return Ok(None);
                };
                self.stream
                    .set_read_timeout(Some(remaining))
                    .context("failed to set Hyprland socket2 read timeout")?;
            } else {
                self.stream
                    .set_read_timeout(None)
                    .context("failed to set Hyprland socket2 read timeout")?;
            }

            let mut chunk = [0_u8; 4096];
            match self.stream.read(&mut chunk) {
                Ok(0) => return Err(anyhow!("Hyprland socket2 closed")),
                Ok(count) => {
                    self.buffer.extend_from_slice(&chunk[..count]);
                    if self.buffer.len() > MAX_SOCKET_FRAME_BYTES && !self.buffer.contains(&b'\n') {
                        self.buffer.clear();
                        return Err(anyhow!(
                            "Hyprland socket2 frame exceeded {MAX_SOCKET_FRAME_BYTES} bytes"
                        ));
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

    fn take_frame(&mut self) -> anyhow::Result<Option<String>> {
        let Some(newline) = self.buffer.iter().position(|byte| *byte == b'\n') else {
            return Ok(None);
        };
        if newline >= MAX_SOCKET_FRAME_BYTES {
            self.buffer.drain(..=newline);
            return Err(anyhow!(
                "Hyprland socket2 frame exceeded {MAX_SOCKET_FRAME_BYTES} bytes"
            ));
        }
        let frame = self.buffer.drain(..=newline).collect::<Vec<_>>();
        String::from_utf8(frame)
            .map(Some)
            .context("Hyprland socket2 frame was not valid UTF-8")
    }
}

pub fn socket2_path_from_env() -> anyhow::Result<PathBuf> {
    let runtime_dir = runtime_dir_from_env()?;
    validate_runtime_ownership(&runtime_dir)?;

    let signature = env::var_os("HYPRLAND_INSTANCE_SIGNATURE");
    resolve_socket2_path_with(&runtime_dir, signature.as_deref(), |path| {
        UnixStream::connect(path).is_ok()
    })
}

pub(crate) fn hyprctl_instance_signature() -> anyhow::Result<OsString> {
    let runtime_dir = runtime_dir_from_env()?;
    validate_runtime_ownership(&runtime_dir)?;
    let configured =
        env::var_os("HYPRLAND_INSTANCE_SIGNATURE").filter(|signature| !signature.is_empty());

    let Some(signature) = configured else {
        let socket = discover_socket2_path(&runtime_dir)?;
        return socket
            .parent()
            .and_then(|path| path.file_name())
            .map(OsStr::to_owned)
            .ok_or_else(|| anyhow!("discovered Hyprland socket2 path has no instance signature"));
    };

    validate_signature(&signature)?;
    validate_signature_directory(&runtime_dir, &signature)?;
    let configured_path = socket2_path(&runtime_dir, &signature);
    if UnixStream::connect(&configured_path).is_ok() {
        return Ok(signature);
    }

    let hypr_dir = runtime_dir.join("hypr");
    match fs::symlink_metadata(&hypr_dir) {
        Ok(_) => validate_hypr_directory(&runtime_dir)?,
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "failed to inspect Hyprland runtime directory {}",
                    hypr_dir.display()
                )
            });
        }
    }

    // A stale but safe signature can still identify the intended instance to
    // hyprctl when no unique live replacement is available.
    match discover_socket2_path_with(&runtime_dir, |path| UnixStream::connect(path).is_ok()) {
        Ok(socket) => socket
            .parent()
            .and_then(|path| path.file_name())
            .map(OsStr::to_owned)
            .ok_or_else(|| anyhow!("discovered Hyprland socket2 path has no instance signature")),
        Err(_) => Ok(signature),
    }
}

pub fn discover_socket2_path(runtime_dir: impl AsRef<Path>) -> anyhow::Result<PathBuf> {
    validate_runtime_ownership(runtime_dir.as_ref())?;
    validate_hypr_directory(runtime_dir.as_ref())?;
    discover_socket2_path_with(runtime_dir, |path| UnixStream::connect(path).is_ok())
}

pub fn resolve_socket2_path_with(
    runtime_dir: impl AsRef<Path>,
    signature: Option<&OsStr>,
    is_socket: impl Fn(&Path) -> bool,
) -> anyhow::Result<PathBuf> {
    let runtime_dir = runtime_dir.as_ref();
    if let Some(signature) = signature.filter(|signature| !signature.is_empty()) {
        validate_signature(signature)?;
        validate_signature_directory(runtime_dir, signature)?;
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
        let entry_path = entry.path();
        let metadata = fs::symlink_metadata(&entry_path).with_context(|| {
            format!(
                "failed to inspect Hyprland instance {}",
                entry_path.display()
            )
        })?;
        if metadata.file_type().is_symlink()
            || !metadata.is_dir()
            || metadata.uid() != current_uid()?
            || metadata.mode() & 0o022 != 0
        {
            continue;
        }
        let socket_path = entry_path.join(".socket2.sock");
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

fn runtime_dir_from_env() -> anyhow::Result<PathBuf> {
    let runtime_dir = env::var_os("XDG_RUNTIME_DIR")
        .ok_or_else(|| anyhow!("XDG_RUNTIME_DIR is not set; cannot locate Hyprland socket2"))?;
    if runtime_dir.is_empty() {
        return Err(anyhow!(
            "XDG_RUNTIME_DIR is empty; cannot locate Hyprland socket2"
        ));
    }
    Ok(runtime_dir.into())
}

pub(crate) fn validate_signature(signature: &OsStr) -> anyhow::Result<()> {
    let path = Path::new(signature);
    let mut components = path.components();
    let valid = matches!(components.next(), Some(Component::Normal(component)) if !component.is_empty())
        && components.next().is_none();
    if !valid || path.is_absolute() {
        return Err(anyhow!(
            "HYPRLAND_INSTANCE_SIGNATURE must be exactly one normal path component"
        ));
    }
    Ok(())
}

fn validate_runtime_ownership(runtime_dir: &Path) -> anyhow::Result<()> {
    let metadata = fs::symlink_metadata(runtime_dir).with_context(|| {
        format!(
            "failed to inspect runtime directory {}",
            runtime_dir.display()
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(anyhow!(
            "runtime directory {} must be a real directory, not a symlink",
            runtime_dir.display()
        ));
    }
    let uid = current_uid()?;
    if metadata.uid() != uid {
        return Err(anyhow!(
            "runtime directory {} is owned by uid {}, expected {uid}",
            runtime_dir.display(),
            metadata.uid()
        ));
    }
    if metadata.mode() & 0o022 != 0 {
        return Err(anyhow!(
            "runtime directory {} must not be group- or world-writable",
            runtime_dir.display()
        ));
    }
    Ok(())
}

fn validate_hypr_directory(runtime_dir: &Path) -> anyhow::Result<()> {
    validate_owned_private_directory(&runtime_dir.join("hypr"), "Hyprland runtime directory")
}

fn validate_owned_private_directory(path: &Path, label: &str) -> anyhow::Result<()> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect {label} {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(anyhow!(
            "{label} {} must be a real directory, not a symlink",
            path.display()
        ));
    }
    if metadata.uid() != current_uid()? {
        return Err(anyhow!(
            "{label} {} has mismatched ownership",
            path.display()
        ));
    }
    if metadata.mode() & 0o022 != 0 {
        return Err(anyhow!(
            "{label} {} must not be group- or world-writable",
            path.display()
        ));
    }
    Ok(())
}

fn validate_signature_directory(runtime_dir: &Path, signature: &OsStr) -> anyhow::Result<()> {
    let directory = runtime_dir.join("hypr").join(signature);
    match fs::symlink_metadata(&directory) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(anyhow!(
            "Hyprland instance directory {} must not be a symlink",
            directory.display()
        )),
        Ok(metadata) if metadata.uid() != current_uid()? => Err(anyhow!(
            "Hyprland instance directory {} has mismatched ownership",
            directory.display()
        )),
        Ok(metadata) if !metadata.is_dir() => Err(anyhow!(
            "Hyprland instance path {} is not a directory",
            directory.display()
        )),
        Ok(metadata) if metadata.mode() & 0o022 != 0 => Err(anyhow!(
            "Hyprland instance directory {} must not be group- or world-writable",
            directory.display()
        )),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => {
            Err(error).with_context(|| format!("failed to inspect {}", directory.display()))
        }
    }
}

fn current_uid() -> anyhow::Result<u32> {
    // SAFETY: geteuid has no preconditions and does not dereference pointers.
    Ok(unsafe { libc::geteuid() })
}

fn validate_socket_inode(path: &Path) -> anyhow::Result<()> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect Hyprland socket2 {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_socket() {
        return Err(anyhow!(
            "Hyprland socket2 {} must be a real Unix socket, not a symlink",
            path.display()
        ));
    }
    if metadata.uid() != current_uid()? {
        return Err(anyhow!(
            "Hyprland socket2 {} has mismatched ownership",
            path.display()
        ));
    }
    if metadata.mode() & 0o022 != 0 {
        return Err(anyhow!(
            "Hyprland socket2 {} must not be group- or world-writable",
            path.display()
        ));
    }
    Ok(())
}

fn validate_peer_credentials(stream: &UnixStream, path: &Path) -> anyhow::Result<()> {
    #[cfg(target_os = "linux")]
    {
        let mut credentials = libc::ucred {
            pid: 0,
            uid: 0,
            gid: 0,
        };
        let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        // SAFETY: all pointers refer to initialized storage of the advertised
        // length, and the file descriptor belongs to the live Unix stream.
        let result = unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                std::ptr::addr_of_mut!(credentials).cast(),
                &mut length,
            )
        };
        if result != 0 {
            return Err(std::io::Error::last_os_error()).with_context(|| {
                format!(
                    "failed to verify Hyprland socket2 peer at {}",
                    path.display()
                )
            });
        }
        let uid = current_uid()?;
        if credentials.uid != uid {
            return Err(anyhow!(
                "Hyprland socket2 peer at {} is uid {}, expected {uid}",
                path.display(),
                credentials.uid
            ));
        }
    }
    Ok(())
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
