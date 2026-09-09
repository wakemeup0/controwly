use std::fmt;
#[cfg(not(windows))]
use std::fs::{self, File, OpenOptions};
use std::io;
#[cfg(not(windows))]
use std::io::{Read, Write};
use std::path::Path;
#[cfg(not(windows))]
use std::path::PathBuf;

#[derive(Debug)]
pub(crate) enum InstanceError {
    Busy,
    Io(io::Error),
    #[cfg(windows)]
    Native(String),
}

impl fmt::Display for InstanceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Busy => write!(f, "another Controwly instance is already running"),
            Self::Io(error) => write!(f, "cannot acquire Controwly instance lock: {error}"),
            #[cfg(windows)]
            Self::Native(error) => {
                write!(f, "cannot acquire native Controwly instance lock: {error}")
            }
        }
    }
}

impl std::error::Error for InstanceError {}

/// A process-owned native guard. Windows uses a named kernel mutex; Unix
/// uses an atomically-created PID file with stale-owner detection. It is held
/// for the entire process lifetime, including the pre-WebView restore CLI.
pub(crate) struct InstanceGuard {
    #[cfg(windows)]
    mutex: *mut std::ffi::c_void,
    #[cfg(not(windows))]
    lock_path: PathBuf,
    #[cfg(not(windows))]
    lock_file: File,
}

unsafe impl Send for InstanceGuard {}
unsafe impl Sync for InstanceGuard {}

impl InstanceGuard {
    pub(crate) fn acquire(data_dir: &Path) -> Result<Self, InstanceError> {
        #[cfg(windows)]
        {
            let _ = data_dir;
            return Self::acquire_windows();
        }
        #[cfg(not(windows))]
        {
            Self::acquire_unix(data_dir)
        }
    }

    #[cfg(windows)]
    fn acquire_windows() -> Result<Self, InstanceError> {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
        use windows_sys::Win32::System::Threading::CreateMutexW;
        let name: Vec<u16> = std::ffi::OsStr::new("Local\\Wakemeup0.Controwly.Instance")
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let mutex = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
        if mutex.is_null() {
            return Err(InstanceError::Native(format!(
                "CreateMutexW failed with Win32 error {}",
                unsafe { GetLastError() }
            )));
        }
        if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
            use windows_sys::Win32::Foundation::CloseHandle;
            unsafe {
                let _ = CloseHandle(mutex);
            }
            return Err(InstanceError::Busy);
        }
        Ok(Self { mutex })
    }

    #[cfg(not(windows))]
    fn acquire_unix(data_dir: &Path) -> Result<Self, InstanceError> {
        fs::create_dir_all(data_dir).map_err(InstanceError::Io)?;
        let lock_path = data_dir.join("instance.lock");
        for attempt in 0..2 {
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&lock_path)
            {
                Ok(mut lock_file) => {
                    let pid = std::process::id();
                    writeln!(lock_file, "{pid}").map_err(InstanceError::Io)?;
                    lock_file.sync_all().map_err(InstanceError::Io)?;
                    return Ok(Self {
                        lock_path,
                        lock_file,
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists && attempt == 0 => {
                    if !owner_is_alive(&lock_path) {
                        let _ = fs::remove_file(&lock_path);
                        continue;
                    }
                    return Err(InstanceError::Busy);
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    return Err(InstanceError::Busy);
                }
                Err(error) => return Err(InstanceError::Io(error)),
            }
        }
        Err(InstanceError::Busy)
    }
}

impl Drop for InstanceGuard {
    fn drop(&mut self) {
        #[cfg(windows)]
        {
            use windows_sys::Win32::Foundation::CloseHandle;
            if !self.mutex.is_null() {
                unsafe {
                    let _ = CloseHandle(self.mutex);
                }
            }
        }
        #[cfg(not(windows))]
        {
            let _ = self.lock_file.sync_all();
            let _ = fs::remove_file(&self.lock_path);
        }
    }
}

#[cfg(not(windows))]
fn owner_is_alive(path: &Path) -> bool {
    let mut contents = String::new();
    if File::open(path)
        .and_then(|mut file| file.read_to_string(&mut contents))
        .is_err()
    {
        return true;
    }
    let Ok(pid) = contents.trim().parse::<u32>() else {
        return true;
    };
    if pid == 0 {
        return true;
    }
    #[cfg(target_os = "linux")]
    {
        PathBuf::from(format!("/proc/{pid}")).exists()
    }
    #[cfg(not(target_os = "linux"))]
    {
        // Unknown Unix process probing is fail-closed; users can remove a
        // stale lock only after verifying no Controwly process is running.
        true
    }
}
