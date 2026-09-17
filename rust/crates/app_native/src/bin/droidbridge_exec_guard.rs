#![deny(unsafe_op_in_unsafe_fn)]

#[cfg(unix)]
mod unix_guard {
    use persistence::{GuardClean, GuardCleanCause, GuardStarted, encode_guard_frame};
    use std::{
        env,
        fs::{self, File},
        io::{Read, Write},
        os::{
            fd::{FromRawFd, RawFd},
            unix::process::CommandExt,
        },
        path::Path,
        process::{Command, ExitCode},
        sync::atomic::{AtomicI32, Ordering},
        thread,
        time::{Duration, Instant},
    };

    static TERMINATION: AtomicI32 = AtomicI32::new(0);

    struct Arguments {
        proof_fd: RawFd,
        lifetime_fd: RawFd,
        program: String,
        arguments: Vec<String>,
    }

    pub fn main() -> ExitCode {
        let values = env::args().skip(1).collect::<Vec<_>>();
        if values.as_slice() == ["--probe-child"] {
            return probe_child();
        }
        if values.first().map(String::as_str) == Some("--probe-owner-death-child")
            && values.len() == 2
        {
            return probe_owner_death_child(&values[1]);
        }
        match parse_arguments(values).and_then(run) {
            Ok(exit) => ExitCode::from(exit),
            Err(()) => ExitCode::from(125),
        }
    }

    fn parse_arguments(values: Vec<String>) -> Result<Arguments, ()> {
        let separator = values.iter().position(|value| value == "--").ok_or(())?;
        if separator != 4
            || values.first().map(String::as_str) != Some("--proof-fd")
            || values.get(2).map(String::as_str) != Some("--lifetime-fd")
        {
            return Err(());
        }
        let proof_fd = values.get(1).ok_or(())?.parse().map_err(|_| ())?;
        let lifetime_fd = values.get(3).ok_or(())?.parse().map_err(|_| ())?;
        if proof_fd < 3 || lifetime_fd < 3 || proof_fd == lifetime_fd {
            return Err(());
        }
        let program = values
            .get(separator + 1)
            .filter(|value| !value.is_empty())
            .ok_or(())?
            .clone();
        Ok(Arguments {
            proof_fd,
            lifetime_fd,
            program,
            arguments: values.into_iter().skip(separator + 2).collect(),
        })
    }

    fn run(arguments: Arguments) -> Result<u8, ()> {
        set_subreaper()?;
        install_handlers()?;
        set_cloexec(arguments.proof_fd)?;
        set_cloexec(arguments.lifetime_fd)?;
        let mut proof = unsafe { File::from_raw_fd(arguments.proof_fd) };
        let mut lifetime = unsafe { File::from_raw_fd(arguments.lifetime_fd) };
        append_frame(
            &mut proof,
            &GuardStarted {
                pid: std::process::id(),
                start_ticks: read_start_ticks(Path::new("/proc/self/stat"))?,
            },
        )?;

        let mut command = Command::new(&arguments.program);
        command.args(&arguments.arguments);
        unsafe {
            command.pre_exec(|| {
                if libc::setpgid(0, 0) != 0
                    || libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) != 0
                {
                    return Err(std::io::Error::last_os_error());
                }
                install_process_group_filter()?;
                Ok(())
            });
        }
        let mut child = command.spawn().map_err(|_| ())?;
        let group = i32::try_from(child.id()).map_err(|_| ())?;
        let (cause, shell_exit_code) = loop {
            let signal = TERMINATION.load(Ordering::SeqCst);
            if signal == libc::SIGTERM || signal == libc::SIGINT {
                break (GuardCleanCause::Cancelled, None);
            }
            if signal == libc::SIGALRM {
                break (GuardCleanCause::Timeout, None);
            }
            if lifetime_closed(&mut lifetime)? {
                break (GuardCleanCause::OwnerLost, None);
            }
            if let Some(status) = child.try_wait().map_err(|_| ())? {
                use std::os::unix::process::ExitStatusExt;
                break (
                    GuardCleanCause::Exited,
                    status
                        .code()
                        .or_else(|| status.signal().map(|value| 128 + value)),
                );
            }
            thread::sleep(Duration::from_millis(20));
        };

        if !cleanup(group)? {
            return Err(());
        }
        append_frame(
            &mut proof,
            &GuardClean {
                shell_exit_code: (cause == GuardCleanCause::Exited)
                    .then_some(shell_exit_code)
                    .flatten(),
                cause,
            },
        )?;
        Ok(shell_exit_code
            .and_then(|value| u8::try_from(value).ok())
            .unwrap_or(0))
    }

    fn cleanup(group: i32) -> Result<bool, ()> {
        signal_group(group, libc::SIGTERM)?;
        let term_deadline = Instant::now() + Duration::from_millis(2_000);
        if reap_until_clean(group, term_deadline)? {
            return Ok(true);
        }
        let kill_deadline = Instant::now() + Duration::from_millis(3_000);
        loop {
            signal_group(group, libc::SIGKILL)?;
            if reap_until_clean(group, Instant::now() + Duration::from_millis(50))? {
                return Ok(true);
            }
            if Instant::now() >= kill_deadline {
                return Ok(false);
            }
        }
    }

    fn signal_group(group: i32, signal: i32) -> Result<(), ()> {
        if unsafe { libc::kill(-group, signal) } != 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(());
            }
        }
        Ok(())
    }

    fn reap_until_clean(group: i32, deadline: Instant) -> Result<bool, ()> {
        loop {
            let no_children = loop {
                let mut status = 0;
                let result = unsafe { libc::waitpid(-1, &mut status, libc::WNOHANG) };
                if result > 0 {
                    continue;
                }
                if result == 0 {
                    break false;
                }
                match std::io::Error::last_os_error().raw_os_error() {
                    Some(libc::ECHILD) => break true,
                    Some(libc::EINTR) => continue,
                    _ => return Err(()),
                }
            };
            if no_children && process_group_gone(group)? {
                return Ok(true);
            }
            if Instant::now() >= deadline {
                return Ok(false);
            }
            thread::sleep(Duration::from_millis(20));
        }
    }

    fn process_group_gone(group: i32) -> Result<bool, ()> {
        if unsafe { libc::kill(-group, 0) } == 0 {
            return Ok(false);
        }
        match std::io::Error::last_os_error().raw_os_error() {
            Some(libc::ESRCH) => Ok(true),
            _ => Err(()),
        }
    }

    #[cfg(any(target_os = "android", target_os = "linux"))]
    fn install_process_group_filter() -> std::io::Result<()> {
        const BPF_LD_W_ABS: u16 = 0x20;
        const BPF_JMP_JEQ_K: u16 = 0x15;
        const BPF_RET_K: u16 = 0x06;
        const SECCOMP_MODE_FILTER: libc::c_ulong = 2;
        const SECCOMP_RET_KILL_PROCESS: u32 = 0x8000_0000;
        const SECCOMP_RET_ERRNO: u32 = 0x0005_0000;
        const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;
        #[cfg(target_arch = "aarch64")]
        const AUDIT_ARCH: u32 = 0xc000_00b7;
        #[cfg(target_arch = "x86_64")]
        const AUDIT_ARCH: u32 = 0xc000_003e;

        #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
        compile_error!("execution guard supports only admitted 64-bit architectures");

        let deny = SECCOMP_RET_ERRNO | u32::try_from(libc::EPERM).unwrap_or(1);
        let mut filter = [
            libc::sock_filter {
                code: BPF_LD_W_ABS,
                jt: 0,
                jf: 0,
                k: 4,
            },
            libc::sock_filter {
                code: BPF_JMP_JEQ_K,
                jt: 1,
                jf: 0,
                k: AUDIT_ARCH,
            },
            libc::sock_filter {
                code: BPF_RET_K,
                jt: 0,
                jf: 0,
                k: SECCOMP_RET_KILL_PROCESS,
            },
            libc::sock_filter {
                code: BPF_LD_W_ABS,
                jt: 0,
                jf: 0,
                k: 0,
            },
            libc::sock_filter {
                code: BPF_JMP_JEQ_K,
                jt: 0,
                jf: 1,
                k: libc::SYS_setpgid as u32,
            },
            libc::sock_filter {
                code: BPF_RET_K,
                jt: 0,
                jf: 0,
                k: deny,
            },
            libc::sock_filter {
                code: BPF_JMP_JEQ_K,
                jt: 0,
                jf: 1,
                k: libc::SYS_setsid as u32,
            },
            libc::sock_filter {
                code: BPF_RET_K,
                jt: 0,
                jf: 0,
                k: deny,
            },
            libc::sock_filter {
                code: BPF_RET_K,
                jt: 0,
                jf: 0,
                k: SECCOMP_RET_ALLOW,
            },
        ];
        let program = libc::sock_fprog {
            len: u16::try_from(filter.len()).expect("filter length fits u16"),
            filter: filter.as_mut_ptr(),
        };
        if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } != 0
            || unsafe {
                libc::prctl(
                    libc::PR_SET_SECCOMP,
                    SECCOMP_MODE_FILTER,
                    std::ptr::addr_of!(program),
                )
            } != 0
        {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    #[cfg(not(any(target_os = "android", target_os = "linux")))]
    fn install_process_group_filter() -> std::io::Result<()> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "seccomp process-group filter is unavailable",
        ))
    }

    fn lifetime_closed(file: &mut File) -> Result<bool, ()> {
        let mut descriptor = libc::pollfd {
            fd: std::os::fd::AsRawFd::as_raw_fd(file),
            events: libc::POLLIN | libc::POLLHUP | libc::POLLERR,
            revents: 0,
        };
        let result = unsafe { libc::poll(&mut descriptor, 1, 0) };
        if result < 0 {
            return Err(());
        }
        if result == 0 {
            return Ok(false);
        }
        let mut byte = [0_u8; 1];
        match file.read(&mut byte) {
            Ok(0) => Ok(true),
            Ok(_) => Ok(false),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(false),
            Err(_) => Err(()),
        }
    }

    fn append_frame(value: &mut File, record: &impl serde::Serialize) -> Result<(), ()> {
        let frame = encode_guard_frame(record).map_err(|_| ())?;
        let current = value.metadata().map_err(|_| ())?.len();
        if current
            .checked_add(frame.len() as u64)
            .is_none_or(|size| size > 4_096)
        {
            return Err(());
        }
        value.write_all(&frame).map_err(|_| ())?;
        value.sync_all().map_err(|_| ())
    }

    fn set_subreaper() -> Result<(), ()> {
        if unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) } == 0 {
            Ok(())
        } else {
            Err(())
        }
    }

    extern "C" fn signal_handler(signal: i32) {
        TERMINATION.store(signal, Ordering::SeqCst);
    }

    fn install_handlers() -> Result<(), ()> {
        for signal in [libc::SIGTERM, libc::SIGINT, libc::SIGALRM] {
            if unsafe { libc::signal(signal, signal_handler as *const () as libc::sighandler_t) }
                == libc::SIG_ERR
            {
                return Err(());
            }
        }
        unblock_termination_signals()
    }

    fn unblock_termination_signals() -> Result<(), ()> {
        let mut signals = unsafe { std::mem::zeroed::<libc::sigset_t>() };
        if unsafe { libc::sigemptyset(&mut signals) } != 0 {
            return Err(());
        }
        for signal in [libc::SIGTERM, libc::SIGINT, libc::SIGALRM] {
            if unsafe { libc::sigaddset(&mut signals, signal) } != 0 {
                return Err(());
            }
        }
        if unsafe { libc::sigprocmask(libc::SIG_UNBLOCK, &signals, std::ptr::null_mut()) } != 0 {
            return Err(());
        }
        Ok(())
    }

    fn set_cloexec(fd: RawFd) -> Result<(), ()> {
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0 {
            Err(())
        } else {
            Ok(())
        }
    }

    fn read_start_ticks(path: &Path) -> Result<u64, ()> {
        let stat = fs::read_to_string(path).map_err(|_| ())?;
        let close = stat.rfind(')').ok_or(())?;
        stat.get(close + 2..)
            .and_then(|tail| tail.split_whitespace().nth(19))
            .and_then(|value| value.parse().ok())
            .filter(|value| *value > 0)
            .ok_or(())
    }

    fn probe_child() -> ExitCode {
        let first = unsafe { libc::fork() };
        if first < 0 {
            return ExitCode::from(1);
        }
        if first > 0 {
            let mut status = 0;
            loop {
                let result = unsafe { libc::waitpid(first, &mut status, 0) };
                if result == first {
                    return if libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0 {
                        ExitCode::SUCCESS
                    } else {
                        ExitCode::from(2)
                    };
                }
                if result < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR)
                {
                    continue;
                }
                return ExitCode::from(3);
            }
        }
        unsafe { libc::_exit(if confinement_is_enforced() { 0 } else { 4 }) };
    }

    fn probe_owner_death_child(marker: &str) -> ExitCode {
        let first = unsafe { libc::fork() };
        if first < 0 {
            return ExitCode::from(1);
        }
        if first > 0 {
            unsafe { libc::sleep(30) };
            return ExitCode::SUCCESS;
        }
        if !confinement_is_enforced() {
            unsafe { libc::_exit(2) };
        }
        if fs::write(marker, b"ready").is_err() {
            unsafe { libc::_exit(3) };
        }
        unsafe { libc::sleep(30) };
        unsafe { libc::_exit(0) };
    }

    fn confinement_is_enforced() -> bool {
        let setpgid = unsafe { libc::setpgid(0, 0) };
        let setpgid_errno = std::io::Error::last_os_error().raw_os_error();
        let setsid = unsafe { libc::setsid() };
        let setsid_errno = std::io::Error::last_os_error().raw_os_error();
        setpgid == -1
            && setpgid_errno == Some(libc::EPERM)
            && setsid == -1
            && setsid_errno == Some(libc::EPERM)
    }
}

#[cfg(unix)]
fn main() -> std::process::ExitCode {
    unix_guard::main()
}

#[cfg(not(unix))]
fn main() -> std::process::ExitCode {
    std::process::ExitCode::from(125)
}
