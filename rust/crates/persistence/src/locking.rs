use crate::io_error;
use domain::DomainError;
use std::{fs, path::Path};

#[cfg(unix)]
use std::os::fd::AsRawFd;
#[cfg(not(unix))]
use std::{
    collections::BTreeSet,
    path::PathBuf,
    sync::{Mutex, OnceLock},
};

pub struct FileLock {
    file: fs::File,
    #[cfg(not(unix))]
    host_path: PathBuf,
}

impl FileLock {
    pub fn acquire(path: &Path) -> Result<Self, DomainError> {
        let mut options = fs::OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(path).map_err(io_error)?;
        #[cfg(unix)]
        {
            let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
            if result != 0 {
                return Err(io_error(std::io::Error::last_os_error()));
            }
            Ok(Self { file })
        }
        #[cfg(not(unix))]
        {
            let host_path = path.to_path_buf();
            loop {
                let mut held = host_locks().lock().expect("host lock registry poisoned");
                if held.insert(host_path.clone()) {
                    break;
                }
                drop(held);
                std::thread::yield_now();
            }
            Ok(Self { file, host_path })
        }
    }

    pub fn try_acquire(path: &Path) -> Result<Option<Self>, DomainError> {
        let mut options = fs::OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(path).map_err(io_error)?;
        #[cfg(unix)]
        {
            let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            if result == 0 {
                return Ok(Some(Self { file }));
            }
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::EWOULDBLOCK) {
                return Ok(None);
            }
            Err(io_error(error))
        }
        #[cfg(not(unix))]
        {
            let host_path = path.to_path_buf();
            let mut held = host_locks().lock().expect("host lock registry poisoned");
            if !held.insert(host_path.clone()) {
                return Ok(None);
            }
            drop(held);
            Ok(Some(Self { file, host_path }))
        }
    }

    pub fn sync(&self) -> Result<(), DomainError> {
        self.file.sync_all().map_err(io_error)
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        #[cfg(unix)]
        let _ = unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
        #[cfg(not(unix))]
        host_locks()
            .lock()
            .expect("host lock registry poisoned")
            .remove(&self.host_path);
    }
}

#[cfg(not(unix))]
fn host_locks() -> &'static Mutex<BTreeSet<PathBuf>> {
    static LOCKS: OnceLock<Mutex<BTreeSet<PathBuf>>> = OnceLock::new();
    LOCKS.get_or_init(|| Mutex::new(BTreeSet::new()))
}
