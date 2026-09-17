//! The Magisk host's own process binding to the companion-reported default network.
//!
//! The framework never binds a daemon process, so the only authority for this host's default
//! network is the authenticated APK companion's own attachment: its
//! `android.net.Network#getNetworkHandle()` fact for the network it runs under, asserted when
//! that changes and when this Magisk host becomes live, and withdrawn with the connection.
//! This host applies that handle to itself through the public NDK call
//! `android_setprocnetwork`, which the platform documents as the native equivalent of
//! `ConnectivityManager#bindProcessToNetwork`: every socket created afterwards carries the
//! bound network and every host-name resolution is limited to it. The same call with
//! `NETWORK_UNSPECIFIED` clears the binding, which is what this host does when the companion —
//! the only authority for the handle — goes away, because the platform states that sockets
//! bound to a disconnected network cease to work by design.
//!
//! The Runtime's `network.default_changed` event plane is deliberately not this fact's path: it
//! carries a subscription-scoped event to Automations and exists only while one requires it,
//! while this process must follow the device's network whenever it is the Magisk host.
//!
//! Every decision below is portable and is driven by a scripted binder, so each transition is
//! exercised without a device. The one device seam is the FFI call, and it reports the
//! platform's own negative-errno failure instead of leaving the process silently unbound.

use contract::ErrorCode;
use domain::DomainError;
use std::sync::Mutex;

/// `NETWORK_UNSPECIFIED`: the handle value that clears a process binding.
const NETWORK_UNSPECIFIED: u64 = 0;

/// The device seam: the daemon installs the platform binder and a test supplies a scripted one.
trait ProcessNetworkBinder: Send + Sync {
    /// Binds this process to `handle`, or clears the binding when `handle` is
    /// `NETWORK_UNSPECIFIED`.
    fn set_process_network(&self, handle: u64) -> Result<(), DomainError>;
}

/// One process binding per host instance. A value is recorded only after the device accepted
/// it, so a refused binding is retried by the next fact that requests it, and an unchanged fact
/// never repeats the call.
pub(crate) struct ProcessNetworkAttachment {
    binder: Box<dyn ProcessNetworkBinder>,
    applied: Mutex<Option<u64>>,
}

impl ProcessNetworkAttachment {
    fn new(binder: Box<dyn ProcessNetworkBinder>) -> Self {
        Self {
            binder,
            applied: Mutex::new(None),
        }
    }

    /// The binder this crate installs on the Magisk host.
    #[cfg(unix)]
    pub(crate) fn device() -> Self {
        Self::new(Box::new(PlatformProcessNetworkBinder))
    }

    /// Applies one companion default-network fact: the reported handle binds this process, and
    /// a fact that reports no default network clears the binding.
    pub(crate) fn apply(&self, network_id: Option<&str>) -> Result<(), DomainError> {
        let requested = handle_from_fact(network_id)?;
        let mut applied = self.applied.lock().map_err(|_| {
            DomainError::new(ErrorCode::InternalError, "process binding lock failed")
        })?;
        if *applied == requested {
            return Ok(());
        }
        self.binder
            .set_process_network(requested.unwrap_or(NETWORK_UNSPECIFIED))?;
        *applied = requested;
        Ok(())
    }

    /// Clears the binding: the host holds no authority for any network once the companion that
    /// reported it is gone.
    pub(crate) fn clear(&self) -> Result<(), DomainError> {
        self.apply(None)
    }
}

/// The companion fact is `Network#getNetworkHandle()` in decimal. `NETWORK_UNSPECIFIED` is the
/// platform's own "no binding" value, so it maps to clearing instead of a second bound state.
fn handle_from_fact(network_id: Option<&str>) -> Result<Option<u64>, DomainError> {
    let Some(fact) = network_id else {
        return Ok(None);
    };
    let handle = fact.parse::<u64>().map_err(|_| {
        DomainError::invalid("companion default-network fact is not a network handle")
    })?;
    Ok((handle != NETWORK_UNSPECIFIED).then_some(handle))
}

#[cfg(unix)]
struct PlatformProcessNetworkBinder;

#[cfg(target_os = "android")]
impl ProcessNetworkBinder for PlatformProcessNetworkBinder {
    fn set_process_network(&self, handle: u64) -> Result<(), DomainError> {
        // SAFETY: the call takes one value and owns no memory.
        let result = unsafe { ffi::android_setprocnetwork(handle) };
        if result == 0 {
            return Ok(());
        }
        // netd answers with a negative errno, so the sign is the platform's own convention.
        Err(attachment_failure(result.saturating_neg()))
    }
}

#[cfg(all(unix, not(target_os = "android")))]
impl ProcessNetworkBinder for PlatformProcessNetworkBinder {
    fn set_process_network(&self, handle: u64) -> Result<(), DomainError> {
        let _ = handle;
        Err(DomainError::new(
            ErrorCode::CapabilityUnavailable,
            "process network attachment is unavailable on this platform",
        ))
    }
}

#[cfg(target_os = "android")]
fn attachment_failure(errno: libc::c_int) -> DomainError {
    match errno {
        libc::EPERM | libc::EACCES => DomainError::new(
            ErrorCode::PermissionDenied,
            "process network attachment is not permitted",
        ),
        libc::ENONET => DomainError::new(
            ErrorCode::NotFound,
            "the reported default network no longer exists",
        ),
        _ => DomainError::new(ErrorCode::IoError, "process network attachment failed"),
    }
}

#[cfg(target_os = "android")]
mod ffi {
    // `android_setprocnetwork`: it binds this process to `network`, or clears the binding with
    // `NETWORK_UNSPECIFIED`. The platform library exports it from API 23 on and the NDK ships
    // it as a stub, so the symbol is resolved when the module is linked, not at first use.
    #[link(name = "android")]
    unsafe extern "C" {
        pub(super) fn android_setprocnetwork(network: u64) -> libc::c_int;
    }
}

#[cfg(test)]
mod tests {
    use super::{NETWORK_UNSPECIFIED, ProcessNetworkAttachment, ProcessNetworkBinder};
    use contract::ErrorCode;
    use domain::DomainError;
    use std::sync::{Arc, Mutex};

    /// One scripted device: it records every call it receives and refuses the calls whose
    /// handle its script names.
    #[derive(Default)]
    struct ScriptedBinder {
        calls: Mutex<Vec<u64>>,
        refused: Mutex<Vec<u64>>,
    }

    impl ScriptedBinder {
        fn refusing(handle: u64) -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                refused: Mutex::new(vec![handle]),
            }
        }

        fn calls(&self) -> Vec<u64> {
            self.calls.lock().expect("calls").clone()
        }
    }

    impl ProcessNetworkBinder for Arc<ScriptedBinder> {
        fn set_process_network(&self, handle: u64) -> Result<(), DomainError> {
            self.calls.lock().expect("calls").push(handle);
            if self.refused.lock().expect("refused").contains(&handle) {
                return Err(DomainError::new(
                    ErrorCode::PermissionDenied,
                    "process network attachment is not permitted",
                ));
            }
            Ok(())
        }
    }

    fn attachment(binder: &Arc<ScriptedBinder>) -> ProcessNetworkAttachment {
        ProcessNetworkAttachment::new(Box::new(Arc::clone(binder)))
    }

    #[test]
    fn i8_net_g18_a_companion_handle_binds_the_process_once_and_clears_it() {
        let device = Arc::new(ScriptedBinder::default());
        let binding = attachment(&device);
        assert!(device.calls().is_empty());

        binding.apply(Some("100")).expect("bind");
        assert_eq!(device.calls(), vec![100]);

        // The framework re-reporting the same default network is not a second binding, and a
        // re-subscription after the companion source changed costs no call either.
        binding.apply(Some("100")).expect("bind");
        assert_eq!(device.calls(), vec![100]);

        binding.apply(Some("101")).expect("rebind");
        assert_eq!(device.calls(), vec![100, 101]);

        // A companion that reports no default network clears the binding with the platform's
        // own unspecified value, and a later clear is not a second call.
        binding.apply(None).expect("clear");
        assert_eq!(device.calls(), vec![100, 101, NETWORK_UNSPECIFIED]);
        binding.clear().expect("clear");
        assert_eq!(device.calls(), vec![100, 101, NETWORK_UNSPECIFIED]);
    }

    #[test]
    fn i8_net_g19_an_unusable_fact_never_becomes_a_binding() {
        let device = Arc::new(ScriptedBinder::default());
        let binding = attachment(&device);

        // The companion's fact is a decimal handle. An interface name is what this daemon's
        // own route-based source produces, so accepting it would bind the process to a value
        // the platform never issued.
        let unusable = binding.apply(Some("tun0")).expect_err("not a handle");
        assert_eq!(unusable.code, ErrorCode::InvalidArgument);
        assert!(device.calls().is_empty());

        // A handle the device refuses leaves no recorded binding, so the next fact that
        // requests it is retried instead of being answered from stale state.
        let refused = Arc::new(ScriptedBinder::refusing(101));
        let binding = attachment(&refused);
        let failure = binding.apply(Some("101")).expect_err("refused");
        assert_eq!(failure.code, ErrorCode::PermissionDenied);
        assert_eq!(refused.calls(), vec![101]);

        let accepted = Arc::new(ScriptedBinder::default());
        let binding = attachment(&accepted);
        binding.apply(Some("101")).expect("retry");
        binding.apply(Some("101")).expect("unchanged");
        assert_eq!(accepted.calls(), vec![101]);

        // `NETWORK_UNSPECIFIED` is the platform's own value for "no binding", so a fact that
        // carries it clears the binding instead of becoming a second bound state.
        let unspecified = Arc::new(ScriptedBinder::default());
        let binding = attachment(&unspecified);
        binding.apply(Some("101")).expect("bind");
        binding.apply(Some("0")).expect("clear");
        binding.clear().expect("already clear");
        assert_eq!(unspecified.calls(), vec![101, NETWORK_UNSPECIFIED]);
    }
}
