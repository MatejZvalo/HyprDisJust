use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

pub const PRIVATE_FILE_MODE: u32 = 0o600;
pub const PRIVATE_DIR_MODE: u32 = 0o700;
pub const PUBLIC_FILE_MODE: u32 = 0o644;

pub fn read_limited(path: &Path, maximum_bytes: u64, label: &str) -> anyhow::Result<Vec<u8>> {
    let mut options = OpenOptions::new();
    options.read(true);
    set_no_follow(&mut options);
    let file = options
        .open(path)
        .with_context(|| format!("failed to open {label} at {}", path.display()))?;
    validate_open_regular_file(&file, path, label, false)?;
    let metadata = file
        .metadata()
        .with_context(|| format!("failed to inspect opened {label} at {}", path.display()))?;
    if metadata.len() > maximum_bytes {
        bail!(
            "{label} at {} is too large ({} bytes; maximum is {maximum_bytes})",
            path.display(),
            metadata.len()
        );
    }
    let capacity = usize::try_from(maximum_bytes.min(8192)).unwrap_or(8192);
    let mut contents = Vec::with_capacity(capacity);
    file.take(maximum_bytes.saturating_add(1))
        .read_to_end(&mut contents)
        .with_context(|| format!("failed to read {label} at {}", path.display()))?;
    if contents.len() as u64 > maximum_bytes {
        bail!(
            "{label} at {} is too large (more than {maximum_bytes} bytes)",
            path.display()
        );
    }
    Ok(contents)
}

pub fn open_private_lock(path: &Path, label: &str) -> anyhow::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    options.mode(PRIVATE_FILE_MODE);
    set_no_follow(&mut options);
    let file = options
        .open(path)
        .with_context(|| format!("failed to open {label} {}", path.display()))?;
    validate_open_regular_file(&file, path, label, true)?;
    #[cfg(unix)]
    file.set_permissions(fs::Permissions::from_mode(PRIVATE_FILE_MODE))
        .with_context(|| format!("failed to secure {label} {}", path.display()))?;
    Ok(file)
}

pub fn open_private_append(path: &Path, label: &str) -> anyhow::Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    options.mode(PRIVATE_FILE_MODE);
    set_no_follow(&mut options);
    let file = options
        .open(path)
        .with_context(|| format!("failed to open {label} {}", path.display()))?;
    validate_open_regular_file(&file, path, label, true)?;
    #[cfg(unix)]
    file.set_permissions(fs::Permissions::from_mode(PRIVATE_FILE_MODE))
        .with_context(|| format!("failed to secure {label} {}", path.display()))?;
    Ok(file)
}

pub fn atomic_write(path: &Path, contents: &[u8], requested_mode: u32) -> anyhow::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    ensure_private_directory(parent)?;
    let target_mode = replacement_mode(path, requested_mode)?;
    let temp_path = unique_temp_path(path);

    let result = (|| -> anyhow::Result<()> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(target_mode);
        let mut temporary = options
            .open(&temp_path)
            .with_context(|| format!("failed to create temporary file {}", temp_path.display()))?;
        #[cfg(unix)]
        temporary
            .set_permissions(fs::Permissions::from_mode(target_mode))
            .with_context(|| format!("failed to set permissions on {}", temp_path.display()))?;
        temporary
            .write_all(contents)
            .with_context(|| format!("failed to write {}", temp_path.display()))?;
        temporary
            .sync_all()
            .with_context(|| format!("failed to sync {}", temp_path.display()))?;
        fs::rename(&temp_path, path).with_context(|| {
            format!(
                "failed to atomically replace {} with {}",
                path.display(),
                temp_path.display()
            )
        })?;
        File::open(parent)
            .with_context(|| format!("failed to open directory {} for syncing", parent.display()))?
            .sync_all()
            .with_context(|| format!("failed to sync directory {}", parent.display()))?;
        Ok(())
    })();

    if result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }
    result
}

pub fn ensure_private_directory(path: &Path) -> anyhow::Result<()> {
    let path = if path.as_os_str().is_empty() {
        Path::new(".")
    } else {
        path
    };

    if !path.is_absolute() {
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        if parent != path {
            ensure_private_directory(parent)?;
        }
    }

    ensure_private_directory_components(path)?;
    validate_private_directory(path)
}

fn ensure_private_directory_components(path: &Path) -> anyhow::Result<()> {
    let mut current = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::Prefix(prefix) => current.push(prefix.as_os_str()),
            std::path::Component::RootDir => current.push(Path::new("/")),
            std::path::Component::CurDir => {
                if current.as_os_str().is_empty() {
                    current.push(Path::new("."));
                }
            }
            std::path::Component::ParentDir => current.push(Path::new("..")),
            std::path::Component::Normal(name) => {
                current.push(name);
                match fs::symlink_metadata(&current) {
                    Ok(metadata) => {
                        if metadata.file_type().is_symlink() || !metadata.is_dir() {
                            bail!(
                                "{} must be a real directory, not a symlink",
                                current.display()
                            );
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        match fs::create_dir(&current) {
                            Ok(()) => {
                                #[cfg(unix)]
                                fs::set_permissions(
                                    &current,
                                    fs::Permissions::from_mode(PRIVATE_DIR_MODE),
                                )
                                .with_context(|| {
                                    format!("failed to secure directory {}", current.display())
                                })?;
                                validate_private_directory(&current)?;
                            }
                            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                                validate_private_directory(&current)?;
                            }
                            Err(error) => {
                                return Err(error).with_context(|| {
                                    format!("failed to create directory {}", current.display())
                                });
                            }
                        }
                    }
                    Err(error) => {
                        return Err(error).with_context(|| {
                            format!("failed to inspect directory {}", current.display())
                        });
                    }
                }
            }
        }
    }
    Ok(())
}

fn validate_private_directory(path: &Path) -> anyhow::Result<()> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect directory {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!("{} must be a real directory, not a symlink", path.display());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        // SAFETY: geteuid has no preconditions and does not dereference pointers.
        let uid = unsafe { libc::geteuid() };
        if metadata.uid() != uid {
            bail!(
                "directory {} is owned by uid {}, expected {uid}",
                path.display(),
                metadata.uid()
            );
        }
        if metadata.mode() & 0o022 != 0 {
            bail!(
                "directory {} must not be group- or world-writable",
                path.display()
            );
        }
    }
    Ok(())
}

fn set_no_follow(options: &mut OpenOptions) {
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
}

fn validate_open_regular_file(
    file: &File,
    path: &Path,
    label: &str,
    require_owner: bool,
) -> anyhow::Result<()> {
    let metadata = file
        .metadata()
        .with_context(|| format!("failed to inspect opened {label} {}", path.display()))?;
    if !metadata.is_file() {
        bail!("{label} at {} is not a regular file", path.display());
    }
    #[cfg(unix)]
    if require_owner {
        use std::os::unix::fs::MetadataExt;
        // SAFETY: geteuid has no preconditions and does not dereference pointers.
        let uid = unsafe { libc::geteuid() };
        if metadata.uid() != uid {
            bail!(
                "{label} at {} is owned by uid {}, expected {uid}",
                path.display(),
                metadata.uid()
            );
        }
    }
    Ok(())
}

fn replacement_mode(path: &Path, requested_mode: u32) -> anyhow::Result<u32> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => {
            #[cfg(unix)]
            {
                Ok(metadata.permissions().mode() & 0o777 & requested_mode)
            }
            #[cfg(not(unix))]
            {
                Ok(requested_mode)
            }
        }
        Ok(metadata) if metadata.file_type().is_symlink() => Ok(requested_mode),
        Ok(_) => bail!("refusing to replace non-file target {}", path.display()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(requested_mode),
        Err(error) => Err(error).with_context(|| format!("failed to inspect {}", path.display())),
    }
}

fn unique_temp_path(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("output");
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    path.with_file_name(format!(".{file_name}.{}.{}.tmp", std::process::id(), nanos))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn newly_created_private_directory_trees_secure_every_component() {
        let temp = tempfile::tempdir().unwrap();
        let nested = temp.path().join("one").join("two");

        ensure_private_directory(&nested).unwrap();

        assert_eq!(
            fs::metadata(temp.path().join("one"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            PRIVATE_DIR_MODE
        );
        assert_eq!(
            fs::metadata(&nested).unwrap().permissions().mode() & 0o777,
            PRIVATE_DIR_MODE
        );
    }
}
