//! Where a daemon that dies without a word leaves the reason on disk. The supervisor owns this
//! file because it owns the daemon's stderr: the daemon writes fd 2 as any process does, and every
//! byte that reaches the pipe lands here under a bound, so a crash loop cannot fill the disk and a
//! silent death cannot go unexplained.

use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

/// The bytes one log file holds before the writer rotates to a fresh file.
pub const LOG_FILE_LIMIT_BYTES: u64 = 65_536;
/// The rotated files kept behind the current one; the oldest is discarded once the chain is full.
pub const LOG_BACKUP_FILES: usize = 2;
/// The file the supervisor writes the daemon's stderr to, inside the log directory it is given.
pub const LOG_FILE_NAME: &str = "daemon-stderr.log";
/// One read from the daemon's stderr: a stalled writer is drained at this size.
const DRAIN_CHUNK_BYTES: usize = 8_192;
/// Owner-only: the log directory and every log file are root's alone.
const DIRECTORY_MODE: u32 = 0o700;
const FILE_MODE: u32 = 0o600;

/// Appends `bytes` to the log in `directory`, rotating first whenever the current file would pass
/// [`LOG_FILE_LIMIT_BYTES`], so every file on disk stays within that limit and the bytes written
/// last always survive: a write larger than the limit fills the files it rotates through and loses
/// only its own oldest part.
pub fn append(directory: &Path, bytes: &[u8]) -> io::Result<()> {
    // Only the log directory itself is created. Its parent is the App's canonical base: when App
    // data is cleared under a running supervisor, recreating that base as root would leave it owned
    // by root, and the daemon, which authenticates the App against that owner, would never connect.
    match fs::create_dir(directory) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    let path = directory.join(LOG_FILE_NAME);
    // The log sits inside the App's private data and is written as root, whose umask may be 0, so
    // the modes are set explicitly; this also narrows a directory or file an earlier version left
    // world-writable.
    restrict(directory, DIRECTORY_MODE)?;
    for index in 0..=LOG_BACKUP_FILES {
        let file = if index == 0 {
            path.clone()
        } else {
            backup_path(&path, index)
        };
        if file.exists() {
            restrict(&file, FILE_MODE)?;
        }
    }
    let mut written = file_len(&path)?;
    let mut file = open_append(&path)?;
    let mut remaining = bytes;
    while !remaining.is_empty() {
        if written >= LOG_FILE_LIMIT_BYTES {
            drop(file);
            rotate(&path)?;
            file = open_append(&path)?;
            written = 0;
        }
        let room = remaining
            .len()
            .min(usize::try_from(LOG_FILE_LIMIT_BYTES - written).unwrap_or(remaining.len()));
        file.write_all(&remaining[..room])?;
        remaining = &remaining[room..];
        written += room as u64;
    }
    Ok(())
}

/// Reads `reader` to its end into the log in `directory`. The daemon's stderr is drained for as
/// long as the daemon writes, so the process never blocks on a full pipe; once a write fails, the
/// remaining bytes are read and dropped — a log that cannot be written must not cost the daemon
/// whose stderr it describes.
pub fn drain(mut reader: impl Read, directory: &Path) {
    let mut chunk = [0_u8; DRAIN_CHUNK_BYTES];
    let mut writable = true;
    loop {
        let read = match reader.read(&mut chunk) {
            Ok(0) => return,
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return,
        };
        if writable && append(directory, &chunk[..read]).is_err() {
            writable = false;
        }
    }
}

fn file_len(path: &Path) -> io::Result<u64> {
    match fs::metadata(path) {
        Ok(metadata) => Ok(metadata.len()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(0),
        Err(error) => Err(error),
    }
}

fn open_append(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(FILE_MODE);
    }
    let file = options.open(path)?;
    restrict(path, FILE_MODE)?;
    Ok(file)
}

#[cfg_attr(not(unix), allow(unused_variables))]
fn restrict(path: &Path, mode: u32) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    }
    Ok(())
}

/// Shifts the chain back by one, discarding the oldest file that the shift would overwrite.
fn rotate(path: &Path) -> io::Result<()> {
    for index in (1..LOG_BACKUP_FILES).rev() {
        let from = backup_path(path, index);
        if from.exists() {
            fs::rename(from, backup_path(path, index + 1))?;
        }
    }
    if path.exists() {
        fs::rename(path, backup_path(path, 1))?;
    }
    Ok(())
}

fn backup_path(path: &Path, index: usize) -> PathBuf {
    let mut shifted = path.as_os_str().to_owned();
    shifted.push(format!(".{index}"));
    PathBuf::from(shifted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::Cursor, path::PathBuf};

    fn working_directory(label: &str) -> PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "droidbridge-supervisor-{label}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&directory);
        directory
    }

    fn files(directory: &Path) -> Vec<PathBuf> {
        let mut found = fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        found.sort();
        found
    }

    fn sizes(directory: &Path) -> Vec<u64> {
        files(directory)
            .iter()
            .map(|path| fs::metadata(path).unwrap().len())
            .collect()
    }

    #[test]
    fn i7_g13_stderr_log_rotates_within_its_limit_and_keeps_the_newest_bytes() {
        let directory = working_directory("stderr-log");
        let path = directory.join(LOG_FILE_NAME);
        let first = vec![b'a'; LOG_FILE_LIMIT_BYTES as usize];
        let second = vec![b'b'; LOG_FILE_LIMIT_BYTES as usize];
        let third = vec![b'c'; LOG_FILE_LIMIT_BYTES as usize];

        append(&directory, &first).unwrap();
        assert_eq!(sizes(&directory), [LOG_FILE_LIMIT_BYTES]);
        assert_eq!(fs::read(&path).unwrap(), first);

        append(&directory, &second).unwrap();
        assert_eq!(
            sizes(&directory),
            [LOG_FILE_LIMIT_BYTES, LOG_FILE_LIMIT_BYTES]
        );
        assert_eq!(fs::read(backup_path(&path, 1)).unwrap(), first);
        assert_eq!(fs::read(&path).unwrap(), second);

        // A third file shifts the whole chain back by one, so the oldest file sits at the end.
        append(&directory, &third).unwrap();
        assert_eq!(sizes(&directory), [LOG_FILE_LIMIT_BYTES; 3]);
        assert_eq!(fs::read(&path).unwrap(), third);
        assert_eq!(fs::read(backup_path(&path, 1)).unwrap(), second);
        assert_eq!(fs::read(backup_path(&path, 2)).unwrap(), first);

        // A fourth file fills the chain: the oldest is discarded, not kept, and the newest bytes
        // that passed the limit are the ones left in front.
        append(&directory, b"tail").unwrap();
        assert_eq!(files(&directory).len(), LOG_BACKUP_FILES + 1);
        assert_eq!(fs::read(&path).unwrap(), b"tail");
        assert_eq!(fs::read(backup_path(&path, 1)).unwrap(), third);
        assert!(
            sizes(&directory)
                .iter()
                .all(|size| *size <= LOG_FILE_LIMIT_BYTES)
        );
        assert!(
            !files(&directory)
                .iter()
                .any(|file| fs::read(file).unwrap() == first)
        );

        fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn stderr_log_is_owner_only_even_where_an_earlier_version_was_not() {
        use std::os::unix::fs::PermissionsExt;
        let directory = working_directory("stderr-modes");
        let path = directory.join(LOG_FILE_NAME);
        fs::create_dir_all(&directory).unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o777)).unwrap();
        fs::write(&path, b"old").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o666)).unwrap();
        fs::write(backup_path(&path, 1), b"older").unwrap();
        fs::set_permissions(backup_path(&path, 1), fs::Permissions::from_mode(0o666)).unwrap();

        append(&directory, b"new").unwrap();

        let mode = |file: &Path| fs::metadata(file).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&directory), 0o700);
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(&backup_path(&path, 1)), 0o600);

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn stderr_log_never_recreates_a_cleared_canonical_base() {
        let base = working_directory("stderr-cleared-base");
        let directory = base.join("logs");

        assert!(append(&directory, b"lost").is_err());
        drain(Cursor::new(b"lost".to_vec()), &directory);

        assert!(!base.exists());
    }

    #[test]
    fn i7_g14_stderr_drain_bounds_a_flood_and_keeps_its_tail() {
        let directory = working_directory("stderr-drain");
        let path = directory.join(LOG_FILE_NAME);
        let mut flood = b"head".to_vec();
        flood.extend(std::iter::repeat_n(b'x', 4 * LOG_FILE_LIMIT_BYTES as usize));
        flood.extend(b"tail");

        drain(Cursor::new(flood), &directory);

        assert_eq!(files(&directory).len(), LOG_BACKUP_FILES + 1);
        assert!(
            sizes(&directory)
                .iter()
                .all(|size| *size <= LOG_FILE_LIMIT_BYTES)
        );
        assert!(fs::read(&path).unwrap().ends_with(b"tail"));
        assert!(
            !files(&directory)
                .iter()
                .any(|file| fs::read(file).unwrap().starts_with(b"head"))
        );

        fs::remove_dir_all(directory).unwrap();
    }
}
