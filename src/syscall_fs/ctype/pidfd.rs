extern crate alloc;
use alloc::sync::Arc;
use axfs::api::{FileIO, OpenFlags};
use axprocess::Process;
use axsync::Mutex;

pub struct PidFd {
    flags: Mutex<OpenFlags>,
    process: Arc<Process>,
}

impl PidFd {
    /// Create a new PidFd
    pub fn new(process: Arc<Process>, flags: OpenFlags) -> Self {
        Self {
            flags: Mutex::new(flags),
            process,
        }
    }
}

impl FileIO for PidFd {
    fn read(&self, _buf: &mut [u8]) -> axerrno::AxResult<usize> {
        Err(axerrno::AxError::Unsupported)
    }
    fn write(&self, _buf: &[u8]) -> axerrno::AxResult<usize> {
        Err(axerrno::AxError::Unsupported)
    }
    fn seek(&self, _pos: axfs::api::SeekFrom) -> axerrno::AxResult<u64> {
        Err(axerrno::AxError::Unsupported)
    }
    /// To check whether the target process is still alive
    fn readable(&self) -> bool {
        self.process.get_zombie()
    }

    fn writable(&self) -> bool {
        false
    }

    fn executable(&self) -> bool {
        false
    }

    fn get_type(&self) -> axfs::api::FileIOType {
        axfs::api::FileIOType::Other
    }

    fn get_status(&self) -> OpenFlags {
        self.flags.lock().clone()
    }

    fn set_status(&self, flags: OpenFlags) -> bool {
        *self.flags.lock() = flags;
        true
    }

    fn set_close_on_exec(&self, is_set: bool) -> bool {
        if is_set {
            // 设置close_on_exec位置
            *self.flags.lock() |= OpenFlags::CLOEXEC;
        } else {
            *self.flags.lock() &= !OpenFlags::CLOEXEC;
        }
        true
    }
}
