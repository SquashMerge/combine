use std::io;

use combined::wiring::fd_limit::{FdLimit, FdLimits};

/// The process's `RLIMIT_NOFILE`, through `getrlimit` and `setrlimit`. They are here and not in
/// the library because the library forbids `unsafe`.
struct ProcessFdLimits;

impl ProcessFdLimits {
    fn read() -> io::Result<libc::rlimit> {
        let mut limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: `limit` is a valid, writable `rlimit` for the duration of the call.
        if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(limit)
    }
}

impl FdLimits for ProcessFdLimits {
    fn get(&self) -> io::Result<FdLimit> {
        let limit = Self::read()?;
        Ok(FdLimit {
            soft: limit.rlim_cur,
            hard: limit.rlim_max,
        })
    }

    fn set_soft(&self, soft: u64) -> io::Result<()> {
        let mut limit = Self::read()?;
        limit.rlim_cur = soft;
        // SAFETY: `limit` is a valid `rlimit` that the call only reads.
        if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limit) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

fn main() -> std::process::ExitCode {
    combined::wiring::main(&ProcessFdLimits)
}
