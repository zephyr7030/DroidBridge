//! The Magisk host's Automation time-wake projection (S-LIFE-003): one nonblocking CLOEXEC
//! CLOCK_REALTIME_ALARM timerfd in the Runtime's Tokio reactor, armed with
//! TFD_TIMER_ABSTIME|TFD_TIMER_CANCEL_ON_SET at the earliest persisted due. It copies no schedule:
//! both an expiry and an ECANCELED wall-clock set only ask the scheduler to rescan canonical truth.

use contract::ErrorCode;
use domain::DomainError;
use runtime::{AutomationWakeDue, AutomationWakeProjection, PortFuture};
use std::{
    io,
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
};
use tokio::io::{Interest, unix::AsyncFd};

pub(crate) struct RealtimeAlarmWake {
    timer: AsyncFd<OwnedFd>,
}

impl RealtimeAlarmWake {
    /// Creates the disarmed timer. It must be called inside the Runtime's Tokio reactor.
    pub(crate) fn new() -> Result<Self, DomainError> {
        let descriptor = unsafe {
            libc::timerfd_create(
                libc::CLOCK_REALTIME_ALARM,
                libc::TFD_NONBLOCK | libc::TFD_CLOEXEC,
            )
        };
        if descriptor < 0 {
            return Err(wake_error("wake alarm timer creation failed"));
        }
        // SAFETY: timerfd_create returned a new descriptor that nothing else owns.
        let timer = unsafe { OwnedFd::from_raw_fd(descriptor) };
        let timer = AsyncFd::with_interest(timer, Interest::READABLE)
            .map_err(|_| wake_error("wake alarm timer registration failed"))?;
        Ok(Self { timer })
    }

    fn settime(&self, flags: libc::c_int, value: libc::timespec) -> Result<(), DomainError> {
        let spec = libc::itimerspec {
            it_interval: libc::timespec {
                tv_sec: 0,
                tv_nsec: 0,
            },
            it_value: value,
        };
        // SAFETY: the descriptor is a live timerfd owned by `self`, and `spec` outlives the call.
        let result = unsafe {
            libc::timerfd_settime(
                self.timer.get_ref().as_raw_fd(),
                flags,
                &spec,
                std::ptr::null_mut(),
            )
        };
        if result != 0 {
            return Err(wake_error("wake alarm timer arm failed"));
        }
        Ok(())
    }
}

impl AutomationWakeProjection for RealtimeAlarmWake {
    fn arm(&self, due: Option<&AutomationWakeDue>) -> Result<bool, DomainError> {
        let Some(due) = due else {
            // An all-zero value disarms; an idle daemon keeps no periodic wake.
            self.settime(
                0,
                libc::timespec {
                    tv_sec: 0,
                    tv_nsec: 0,
                },
            )?;
            return Ok(true);
        };
        // A due at or before the epoch cannot be persisted; clamp to the smallest armed value so
        // an all-zero it_value never silently disarms an expired due.
        let millis = due.unix_millis.max(1);
        let seconds = libc::time_t::try_from(millis.div_euclid(1_000))
            .map_err(|_| DomainError::new(ErrorCode::ResourceLimit, "wake due is out of range"))?;
        let nanos = libc::c_long::try_from(millis.rem_euclid(1_000) * 1_000_000)
            .map_err(|_| DomainError::new(ErrorCode::ResourceLimit, "wake due is out of range"))?;
        self.settime(
            libc::TFD_TIMER_ABSTIME | libc::TFD_TIMER_CANCEL_ON_SET,
            libc::timespec {
                tv_sec: seconds,
                tv_nsec: nanos,
            },
        )?;
        Ok(true)
    }

    fn wait<'a>(&'a self) -> PortFuture<'a, Result<(), DomainError>> {
        Box::pin(async move {
            loop {
                let mut ready = self
                    .timer
                    .readable()
                    .await
                    .map_err(|_| wake_error("wake alarm timer wait failed"))?;
                let mut expirations = [0_u8; 8];
                // The read consumes the delivery synchronously after readiness, so dropping this
                // future at its only await point loses nothing.
                match ready.try_io(|inner| {
                    // SAFETY: the descriptor is live and `expirations` is a writable 8-byte buffer.
                    let read = unsafe {
                        libc::read(
                            inner.get_ref().as_raw_fd(),
                            expirations.as_mut_ptr().cast(),
                            expirations.len(),
                        )
                    };
                    if read < 0 {
                        Err(io::Error::last_os_error())
                    } else {
                        Ok(read)
                    }
                }) {
                    Ok(Ok(8)) => return Ok(()),
                    // The wall clock was set while armed: canonical truth is rescanned.
                    Ok(Err(error)) if error.raw_os_error() == Some(libc::ECANCELED) => {
                        return Ok(());
                    }
                    Ok(Ok(_)) | Ok(Err(_)) => {
                        return Err(wake_error("wake alarm timer read failed"));
                    }
                    Err(_would_block) => {}
                }
            }
        })
    }
}

fn wake_error(message: &'static str) -> DomainError {
    DomainError::new(ErrorCode::IoError, message)
}
