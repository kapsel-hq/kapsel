//! Descriptor-relative, collision-safe receipt publication for effect-gateway.
//!
//! Publication accepts only a pre-existing owner-private output directory. It walks path components
//! without following symlinks, writes a same-directory owner-private pending file, installs with a
//! no-replace hard link, verifies inode identity, and syncs the directory before success.

use std::{
    error::Error,
    ffi::{OsStr, OsString},
    fmt,
    fs::File,
    io::{self, Read, Write},
    os::unix::fs::MetadataExt,
    path::{Component, Path, PathBuf},
};

use rustix::fs::{fchmod, fstat, linkat, openat, statat, unlinkat, AtFlags, Mode, OFlags, CWD};
use sha2::{Digest, Sha256};

use super::RECEIPT_BYTES_MAX;

pub(crate) fn receipt_digest_hex(receipt: &[u8]) -> String {
    let digest = Sha256::digest(receipt);
    let mut output = String::with_capacity(64);
    for byte in digest {
        output.push(hex_digit(byte >> 4));
        output.push(hex_digit(byte & 0x0f));
    }
    output
}

pub(crate) fn receipt_filename(operation_id: &str, digest: &str) -> String {
    format!("kap0038-{operation_id}-{digest}.receipt")
}

pub(crate) fn validate_private_directory(path: &Path) -> Result<(), PublicationError> {
    open_parent(&path.join(".kap0038-directory-check")).map(|_| ())
}

#[cfg(feature = "demo-harness")]
pub(in crate::gateway) fn create_private_file(
    path: &Path,
    bytes: &[u8],
) -> Result<(), PublicationError> {
    let (directory, name) = open_parent(path)?;
    create_new_file(&directory, &name, bytes)?;
    directory.sync_all().map_err(PublicationError::Io)
}

pub(crate) fn publish_receipt(path: &Path, receipt: &[u8]) -> Result<(), PublicationError> {
    if receipt.len() > RECEIPT_BYTES_MAX {
        return Err(PublicationError::LimitExceeded);
    }
    let (directory, destination) = open_parent(path)?;
    let pending = pending_name(&destination)?;
    let source = create_or_recover_pending(&directory, &pending, receipt)?;

    install_pending(&directory, &pending, &destination, &source, receipt, || {})?;
    unlink_named_file(&directory, &pending, &source)?;
    directory.sync_all().map_err(PublicationError::Io)
}

#[cfg(test)]
pub(crate) fn read_receipt(path: &Path) -> Result<Vec<u8>, PublicationError> {
    read_receipt_before_read(path, || {})
}

#[cfg(test)]
fn read_receipt_before_read(
    path: &Path,
    before_read: impl FnOnce(),
) -> Result<Vec<u8>, PublicationError> {
    let (directory, name) = open_parent(path)?;
    let file = match open_regular(&directory, &name) {
        Err(PublicationError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
            return Err(PublicationError::MissingDestination);
        },
        result => result?,
    };
    require_private_file(&file)?;
    require_single_link(&file)?;
    require_named_file(&directory, &name, &file)?;
    before_read();
    read_bounded(&file, RECEIPT_BYTES_MAX)
}

fn create_or_recover_pending(
    directory: &File,
    name: &OsStr,
    receipt: &[u8],
) -> Result<File, PublicationError> {
    match create_new_file(directory, name, receipt) {
        Ok(file) => Ok(file),
        Err(PublicationError::Io(error)) if error.kind() == io::ErrorKind::AlreadyExists => {
            let stale = open_regular(directory, name)?;
            require_private_file(&stale)?;
            if require_contents(&stale, receipt).is_ok() {
                stale.sync_all().map_err(PublicationError::Io)?;
                return Ok(stale);
            }
            unlink_named_file(directory, name, &stale)?;
            directory.sync_all().map_err(PublicationError::Io)?;
            create_new_file(directory, name, receipt)
        },
        Err(error) => Err(error),
    }
}

fn install_pending(
    directory: &File,
    pending: &OsStr,
    destination: &OsStr,
    source: &File,
    receipt: &[u8],
    before_link: impl FnOnce(),
) -> Result<(), PublicationError> {
    require_named_file(directory, pending, source)?;
    before_link();
    match linkat(directory, pending, directory, destination, AtFlags::empty()) {
        Ok(()) => {
            if require_named_file(directory, destination, source).is_err() {
                let _ = unlinkat(directory, destination, AtFlags::empty());
                directory.sync_all().map_err(PublicationError::Io)?;
                return Err(PublicationError::UnsafePath);
            }
            Ok(())
        },
        Err(rustix::io::Errno::EXIST) => {
            let existing = open_regular(directory, destination)?;
            require_private_file(&existing)?;
            require_named_file(directory, destination, &existing)?;
            require_contents(&existing, receipt)?;
            existing.sync_all().map_err(PublicationError::Io)
        },
        Err(error) => Err(io_error(error)),
    }
}

fn open_parent(path: &Path) -> Result<(File, OsString), PublicationError> {
    let mut names = Vec::new();
    let mut absolute = false;
    for component in path.components() {
        match component {
            Component::RootDir => absolute = true,
            Component::CurDir => {},
            Component::Normal(name) => names.push(name.to_os_string()),
            Component::ParentDir | Component::Prefix(_) => {
                return Err(PublicationError::UnsafePath);
            },
        }
    }
    let destination = names.pop().ok_or(PublicationError::UnsafePath)?;
    let descriptor_root = descriptor_root(&names, absolute)?;
    let (mut directory, consumed) = if let Some(root) = descriptor_root {
        (
            File::from(
                openat(CWD, &root, descriptor_directory_flags(), Mode::empty())
                    .map_err(io_error)?,
            ),
            4,
        )
    } else {
        let start = if absolute {
            OsStr::new("/")
        } else {
            OsStr::new(".")
        };
        (
            File::from(openat(CWD, start, directory_flags(), Mode::empty()).map_err(io_error)?),
            0,
        )
    };
    for name in names.into_iter().skip(consumed) {
        directory = File::from(
            openat(&directory, name, directory_flags(), Mode::empty()).map_err(io_error)?,
        );
    }
    let metadata = directory.metadata().map_err(PublicationError::Io)?;
    if metadata.uid() != rustix::process::geteuid().as_raw() || metadata.mode() & 0o7777 != 0o700 {
        return Err(PublicationError::UnsafePath);
    }
    Ok((directory, destination))
}

fn descriptor_root(
    names: &[OsString],
    absolute: bool,
) -> Result<Option<PathBuf>, PublicationError> {
    if !absolute || names.len() < 4 || names[0] != "proc" || names[1] != "self" || names[2] != "fd"
    {
        return Ok(None);
    }
    let descriptor = names[3]
        .to_str()
        .filter(|value| !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
        .ok_or(PublicationError::UnsafePath)?;
    Ok(Some(Path::new("/proc/self/fd").join(descriptor)))
}

fn pending_name(destination: &OsStr) -> Result<OsString, PublicationError> {
    let mut name = destination.to_os_string();
    name.push(".pending");
    if name.len() > 255 {
        return Err(PublicationError::UnsafePath);
    }
    Ok(name)
}

fn create_new_file(
    directory: &File,
    name: &OsStr,
    receipt: &[u8],
) -> Result<File, PublicationError> {
    let mut file =
        File::from(openat(directory, name, write_new_flags(), file_mode()).map_err(io_error)?);
    fchmod(&file, file_mode()).map_err(io_error)?;
    require_private_file(&file)?;
    file.write_all(receipt).map_err(PublicationError::Io)?;
    file.sync_all().map_err(PublicationError::Io)?;
    require_named_file(directory, name, &file)?;
    Ok(file)
}

fn open_regular(directory: &File, name: &OsStr) -> Result<File, PublicationError> {
    let file = File::from(openat(directory, name, read_flags(), Mode::empty()).map_err(io_error)?);
    if !file.metadata().map_err(PublicationError::Io)?.is_file() {
        return Err(PublicationError::UnsafePath);
    }
    Ok(file)
}

fn require_private_file(file: &File) -> Result<(), PublicationError> {
    let metadata = file.metadata().map_err(PublicationError::Io)?;
    if metadata.uid() != rustix::process::geteuid().as_raw() || metadata.mode() & 0o7777 != 0o600 {
        return Err(PublicationError::UnsafePath);
    }
    Ok(())
}

#[cfg(test)]
fn require_single_link(file: &File) -> Result<(), PublicationError> {
    if file.metadata().map_err(PublicationError::Io)?.nlink() != 1 {
        return Err(PublicationError::UnsafePath);
    }
    Ok(())
}

fn require_named_file(directory: &File, name: &OsStr, file: &File) -> Result<(), PublicationError> {
    let descriptor = fstat(file).map_err(io_error)?;
    let named = statat(directory, name, AtFlags::SYMLINK_NOFOLLOW).map_err(io_error)?;
    if descriptor.st_dev != named.st_dev || descriptor.st_ino != named.st_ino {
        return Err(PublicationError::UnsafePath);
    }
    Ok(())
}

fn unlink_named_file(directory: &File, name: &OsStr, file: &File) -> Result<(), PublicationError> {
    require_named_file(directory, name, file)?;
    unlinkat(directory, name, AtFlags::empty()).map_err(io_error)
}

fn require_contents(file: &File, receipt: &[u8]) -> Result<(), PublicationError> {
    let metadata = file.metadata().map_err(PublicationError::Io)?;
    if metadata.len() != receipt.len() as u64 {
        return Err(PublicationError::Collision);
    }
    if read_bounded(file, RECEIPT_BYTES_MAX)? == receipt {
        Ok(())
    } else {
        Err(PublicationError::Collision)
    }
}

fn read_bounded(file: &File, maximum_bytes: usize) -> Result<Vec<u8>, PublicationError> {
    let metadata = file.metadata().map_err(PublicationError::Io)?;
    if metadata.len() > maximum_bytes as u64 {
        return Err(PublicationError::LimitExceeded);
    }
    let capacity = usize::try_from(metadata.len()).map_err(|_| PublicationError::LimitExceeded)?;
    let mut bytes = Vec::with_capacity(capacity);
    file.try_clone()
        .map_err(PublicationError::Io)?
        .take(maximum_bytes as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(PublicationError::Io)?;
    if bytes.len() > maximum_bytes {
        return Err(PublicationError::LimitExceeded);
    }
    Ok(bytes)
}

fn directory_flags() -> OFlags {
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
}

fn descriptor_directory_flags() -> OFlags {
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC
}

fn read_flags() -> OFlags {
    OFlags::RDONLY | OFlags::NONBLOCK | OFlags::NOFOLLOW | OFlags::CLOEXEC
}

fn write_new_flags() -> OFlags {
    OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC
}

fn file_mode() -> Mode {
    Mode::RUSR | Mode::WUSR
}

fn io_error(error: rustix::io::Errno) -> PublicationError {
    PublicationError::Io(error.into())
}

#[derive(Debug)]
pub(crate) enum PublicationError {
    Io(io::Error),
    Collision,
    LimitExceeded,
    UnsafePath,
    #[cfg(test)]
    MissingDestination,
}

impl fmt::Display for PublicationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let class = match self {
            Self::Io(_) => "io",
            Self::Collision => "collision",
            Self::LimitExceeded => "limit_exceeded",
            Self::UnsafePath => "unsafe_path",
            #[cfg(test)]
            Self::MissingDestination => "missing_destination",
        };
        write!(
            formatter,
            "effect-gateway receipt publication failure: {class}"
        )
    }
}

impl Error for PublicationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            #[cfg(test)]
            Self::MissingDestination => None,
            Self::Collision | Self::LimitExceeded | Self::UnsafePath => None,
        }
    }
}

fn hex_digit(value: u8) -> char {
    match value {
        0..=9 => char::from(b'0' + value),
        10..=15 => char::from(b'a' + value - 10),
        _ => '?',
    }
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "linux")]
    use std::os::fd::AsRawFd;
    use std::{
        fs,
        os::unix::fs::{symlink, PermissionsExt},
        path::PathBuf,
        process::Command,
    };

    use super::*;

    const RESTRICTIVE_UMASK_TEST: &str = concat!(
        "gateway::receipt::publication::tests::",
        "creation_forces_exact_mode_under_restrictive_umask"
    );

    fn directory(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("k3-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        fs::canonicalize(path).unwrap()
    }

    #[test]
    fn requires_preexisting_owner_private_directory_and_private_files() {
        let root = directory("private");
        let missing = root.join("missing/receipt");
        assert!(publish_receipt(&missing, b"receipt").is_err());
        fs::set_permissions(&root, fs::Permissions::from_mode(0o750)).unwrap();
        assert!(matches!(
            publish_receipt(&root.join("receipt"), b"receipt"),
            Err(PublicationError::UnsafePath)
        ));
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let path = root.join("receipt");
        publish_receipt(&path, b"receipt").unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_intermediate_final_and_pending_symlinks() {
        let root = directory("symlinks");
        let real = root.join("real");
        fs::create_dir(&real).unwrap();
        fs::set_permissions(&real, fs::Permissions::from_mode(0o700)).unwrap();
        let linked = root.join("linked");
        symlink(&real, &linked).unwrap();
        assert!(publish_receipt(&linked.join("receipt"), b"receipt").is_err());

        let target = real.join("target");
        fs::write(&target, b"receipt").unwrap();
        symlink(&target, real.join("receipt")).unwrap();
        assert!(publish_receipt(&real.join("receipt"), b"receipt").is_err());
        symlink(&target, real.join("other.pending")).unwrap();
        assert!(publish_receipt(&real.join("other"), b"receipt").is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn proc_descriptor_root_keeps_publication_out_of_replacement_directory() {
        let root = directory("proc-descriptor");
        let handle = File::open(&root).unwrap();
        let access = PathBuf::from(format!("/proc/self/fd/{}", handle.as_raw_fd()));
        let retained = root.with_extension("retained");
        fs::rename(&root, &retained).unwrap();
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();

        publish_receipt(&access.join("receipt"), b"receipt").unwrap();

        assert_eq!(fs::read(retained.join("receipt")).unwrap(), b"receipt");
        assert!(!root.join("receipt").exists());
        drop(handle);
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(retained).unwrap();
    }

    #[test]
    fn stale_partial_pending_is_cleaned_and_exact_publication_recovers() {
        let root = directory("partial-pending");
        let path = root.join("receipt");
        fs::write(root.join("receipt.pending"), b"partial").unwrap();
        fs::set_permissions(
            root.join("receipt.pending"),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        publish_receipt(&path, b"complete-receipt").unwrap();
        assert_eq!(read_receipt(&path).unwrap(), b"complete-receipt");
        assert!(!root.join("receipt.pending").exists());
        publish_receipt(&path, b"complete-receipt").unwrap();
        assert_eq!(read_receipt(&path).unwrap(), b"complete-receipt");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn crash_after_install_before_pending_cleanup_recovers_exact_bytes() {
        let root = directory("installed-pending");
        let destination_path = root.join("receipt");
        let (directory, destination) = open_parent(&destination_path).unwrap();
        let pending = pending_name(&destination).unwrap();
        let source = create_new_file(&directory, &pending, b"receipt").unwrap();
        install_pending(
            &directory,
            &pending,
            &destination,
            &source,
            b"receipt",
            || {},
        )
        .unwrap();
        drop(source);
        drop(directory);
        assert!(root.join("receipt.pending").exists());

        publish_receipt(&destination_path, b"receipt").unwrap();

        assert_eq!(read_receipt(&destination_path).unwrap(), b"receipt");
        assert!(!root.join("receipt.pending").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn oversized_special_and_different_destinations_fail_closed() {
        let root = directory("destinations");
        let oversized = root.join("oversized");
        fs::write(&oversized, vec![0_u8; RECEIPT_BYTES_MAX + 1]).unwrap();
        fs::set_permissions(&oversized, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(matches!(
            read_receipt(&oversized),
            Err(PublicationError::LimitExceeded)
        ));

        let different = root.join("different");
        fs::write(&different, b"different").unwrap();
        fs::set_permissions(&different, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(matches!(
            publish_receipt(&different, b"receipt"),
            Err(PublicationError::Collision)
        ));
        assert_eq!(fs::read(different).unwrap(), b"different");

        let socket = root.join("socket");
        let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        assert!(read_receipt(&socket).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn receipt_reads_require_exact_modes_and_a_single_link() {
        let root = directory("read-identity");
        let path = root.join("receipt");
        publish_receipt(&path, b"receipt").unwrap();

        fs::set_permissions(&path, fs::Permissions::from_mode(0o400)).unwrap();
        assert!(matches!(
            read_receipt(&path),
            Err(PublicationError::UnsafePath)
        ));
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();

        let linked = root.join("linked");
        fs::hard_link(&path, &linked).unwrap();
        assert!(matches!(
            read_receipt(&path),
            Err(PublicationError::UnsafePath)
        ));
        fs::remove_file(linked).unwrap();

        fs::set_permissions(&root, fs::Permissions::from_mode(0o500)).unwrap();
        assert!(matches!(
            read_receipt(&path),
            Err(PublicationError::UnsafePath)
        ));
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(read_receipt(&path).unwrap(), b"receipt");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn creation_forces_exact_mode_under_restrictive_umask() {
        const CHILD: &str = "KAPSEL_RESTRICTIVE_UMASK_CHILD";
        const MARKER: &str = "KAPSEL_RESTRICTIVE_UMASK_MARKER";
        if std::env::var_os(CHILD).is_some() {
            rustix::process::umask(Mode::WUSR);
            let root = directory("restrictive-umask");
            let path = root.join("receipt");
            publish_receipt(&path, b"receipt").unwrap();
            assert_eq!(fs::metadata(&path).unwrap().mode() & 0o7777, 0o600);
            assert_eq!(read_receipt(&path).unwrap(), b"receipt");
            fs::remove_dir_all(root).unwrap();
            fs::write(std::env::var_os(MARKER).unwrap(), b"completed").unwrap();
            return;
        }

        let marker = std::env::temp_dir().join(format!(
            "kapsel-restrictive-umask-marker-{}",
            std::process::id()
        ));
        let _ = fs::remove_file(&marker);
        let status = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", RESTRICTIVE_UMASK_TEST])
            .env(CHILD, "1")
            .env(MARKER, &marker)
            .status()
            .unwrap();
        assert!(status.success());
        assert_eq!(fs::read(&marker).unwrap(), b"completed");
        fs::remove_file(marker).unwrap();
    }

    #[test]
    fn fifo_and_unsafe_read_paths_fail_without_blocking() {
        let root = directory("fifo-read");
        let fifo = root.join("receipt-fifo");
        assert!(Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success());
        assert!(matches!(
            read_receipt(&fifo),
            Err(PublicationError::UnsafePath)
        ));

        let real = root.join("real");
        fs::create_dir(&real).unwrap();
        fs::set_permissions(&real, fs::Permissions::from_mode(0o700)).unwrap();
        let path = real.join("receipt");
        publish_receipt(&path, b"receipt").unwrap();
        symlink(&real, root.join("linked")).unwrap();
        assert!(read_receipt(&root.join("linked/receipt")).is_err());
        assert!(read_receipt(&root.join("real/../real/receipt")).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn read_uses_the_validated_open_descriptor_after_name_replacement() {
        let root = directory("read-substitution");
        let path = root.join("receipt");
        let retained = root.join("retained");
        publish_receipt(&path, b"original").unwrap();

        let bytes = read_receipt_before_read(&path, || {
            fs::rename(&path, &retained).unwrap();
            fs::write(&path, b"replacement").unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        })
        .unwrap();

        assert_eq!(bytes, b"original");
        assert_eq!(fs::read(path).unwrap(), b"replacement");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn missing_destination_is_distinct_from_missing_or_unsafe_parent() {
        let root = directory("missing");
        assert!(matches!(
            read_receipt(&root.join("receipt")),
            Err(PublicationError::MissingDestination)
        ));
        assert!(matches!(
            read_receipt(&root.join("missing/receipt")),
            Err(PublicationError::Io(_))
        ));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn pending_name_substitution_before_install_is_rejected() {
        let root = directory("substitution");
        let destination_path = root.join("receipt");
        let target = root.join("target");
        fs::write(&target, b"receipt").unwrap();
        let (directory, destination) = open_parent(&destination_path).unwrap();
        let pending = pending_name(&destination).unwrap();
        let source = create_new_file(&directory, &pending, b"receipt").unwrap();
        let pending_path = root.join(&pending);
        let result = install_pending(
            &directory,
            &pending,
            &destination,
            &source,
            b"receipt",
            || {
                fs::remove_file(&pending_path).unwrap();
                symlink(&target, &pending_path).unwrap();
            },
        );
        assert!(matches!(result, Err(PublicationError::UnsafePath)));
        assert!(!destination_path.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn parent_components_are_rejected() {
        assert!(matches!(
            publish_receipt(Path::new("receipts/../receipt"), b"receipt"),
            Err(PublicationError::UnsafePath)
        ));
    }
}
