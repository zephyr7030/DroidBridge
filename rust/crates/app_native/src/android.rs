//! APK-hosted Android mother-tool primitives (S-ANDROID-005/006, S-SHIZUKU-006).

use contract::{AndroidIntentInput, AndroidLaunchInput, ErrorCode};
use domain::DomainError;
use runtime::{
    AdmittedExecution, AndroidExecutionDispatch, AndroidNotificationIdentity,
    AndroidNotificationRecord, AndroidPrimitivePort, AndroidPrimitiveResult, ExecutionFailure,
    FrameworkPackageInspection, LocalExecutionClaim, PrivilegedPackageRecord, ProviderToken,
};

const FRAMEWORK_KEY: &str = "android.framework";
const SHIZUKU_KEY: &str = "shizuku.shell";
const NOTIFICATION_LISTENER_KEY: &str = "android.notification_listener";

/// The `:runtime` process name is `<package>:runtime`; its package prefix is the
/// running DroidBridge package that force-stop must never target.
pub(crate) fn running_package() -> Result<String, DomainError> {
    let unavailable = || {
        DomainError::new(
            ErrorCode::InternalError,
            "running package identity is unavailable",
        )
    };
    let cmdline = std::fs::read("/proc/self/cmdline").map_err(|_| unavailable())?;
    let process = cmdline.split(|byte| *byte == 0).next().unwrap_or_default();
    let process = std::str::from_utf8(process).map_err(|_| unavailable())?;
    let package = process.split(':').next().unwrap_or_default();
    runtime::validate_package_name(package).map_err(|_| unavailable())?;
    Ok(package.to_owned())
}

struct KeyedDispatch(&'static str);

impl AndroidExecutionDispatch for KeyedDispatch {
    fn dispatch(
        &self,
        primitive: &str,
        payload: &[u8],
        execution: &AdmittedExecution,
    ) -> Result<AndroidPrimitiveResult, DomainError> {
        crate::dispatch_android_execution_for(self.0, primitive, payload, execution)
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ApkAndroidPort;

fn provider(
    execution: &AdmittedExecution,
    expected: ProviderToken,
    claim: &LocalExecutionClaim,
) -> Result<(), ExecutionFailure> {
    claim.checkpoint().map_err(clean_failure)?;
    if execution.executor.provider != expected {
        return Err(clean_failure(DomainError::new(
            ErrorCode::StaleAuthority,
            "Android primitive provider is not bound to this adapter",
        )));
    }
    Ok(())
}

fn clean_failure(error: DomainError) -> ExecutionFailure {
    ExecutionFailure {
        error,
        cleanup_verified: true,
    }
}

impl AndroidPrimitivePort for ApkAndroidPort {
    fn framework_package_inspect(
        &self,
        execution: &AdmittedExecution,
        package_name: &str,
        claim: &LocalExecutionClaim,
    ) -> Result<FrameworkPackageInspection, ExecutionFailure> {
        provider(execution, ProviderToken::AppFramework, claim)?;
        runtime::bridge_framework_package_inspect(
            &KeyedDispatch(FRAMEWORK_KEY),
            execution,
            package_name,
        )
        .map_err(clean_failure)
    }

    fn package_inventory(
        &self,
        execution: &AdmittedExecution,
        include_system: bool,
        claim: &LocalExecutionClaim,
    ) -> Result<Vec<PrivilegedPackageRecord>, ExecutionFailure> {
        provider(execution, ProviderToken::Shizuku, claim)?;
        runtime::bridge_shizuku_package_inventory(
            &KeyedDispatch(SHIZUKU_KEY),
            execution,
            include_system,
        )
        .map_err(clean_failure)
    }

    fn force_stop(
        &self,
        execution: &AdmittedExecution,
        package_name: &str,
        claim: &LocalExecutionClaim,
    ) -> Result<(), ExecutionFailure> {
        provider(execution, ProviderToken::Shizuku, claim)?;
        runtime::bridge_shizuku_force_stop(&KeyedDispatch(SHIZUKU_KEY), execution, package_name)
            .map_err(clean_failure)
    }

    fn launch(
        &self,
        execution: &AdmittedExecution,
        input: &AndroidLaunchInput,
        claim: &LocalExecutionClaim,
    ) -> Result<(), ExecutionFailure> {
        provider(execution, ProviderToken::AppFramework, claim)?;
        runtime::bridge_launch(&KeyedDispatch(FRAMEWORK_KEY), execution, input)
            .map_err(clean_failure)
    }

    fn start_intent(
        &self,
        execution: &AdmittedExecution,
        input: &AndroidIntentInput,
        claim: &LocalExecutionClaim,
    ) -> Result<(), ExecutionFailure> {
        provider(execution, ProviderToken::AppFramework, claim)?;
        runtime::bridge_start_intent(&KeyedDispatch(FRAMEWORK_KEY), execution, input)
            .map_err(clean_failure)
    }

    fn clipboard_read(
        &self,
        execution: &AdmittedExecution,
        claim: &LocalExecutionClaim,
    ) -> Result<Option<String>, ExecutionFailure> {
        provider(execution, ProviderToken::AppFramework, claim)?;
        runtime::bridge_clipboard_read(&KeyedDispatch(FRAMEWORK_KEY), execution)
            .map_err(clean_failure)
    }

    fn clipboard_write(
        &self,
        execution: &AdmittedExecution,
        text: &str,
        claim: &LocalExecutionClaim,
    ) -> Result<(), ExecutionFailure> {
        provider(execution, ProviderToken::AppFramework, claim)?;
        runtime::bridge_clipboard_write(&KeyedDispatch(FRAMEWORK_KEY), execution, text)
            .map_err(clean_failure)
    }

    fn clipboard_clear(
        &self,
        execution: &AdmittedExecution,
        claim: &LocalExecutionClaim,
    ) -> Result<(), ExecutionFailure> {
        provider(execution, ProviderToken::AppFramework, claim)?;
        runtime::bridge_clipboard_clear(&KeyedDispatch(FRAMEWORK_KEY), execution)
            .map_err(clean_failure)
    }

    fn notification_snapshot(
        &self,
        execution: &AdmittedExecution,
        claim: &LocalExecutionClaim,
    ) -> Result<Vec<AndroidNotificationRecord>, ExecutionFailure> {
        provider(execution, ProviderToken::NotificationListener, claim)?;
        runtime::bridge_notification_snapshot(&KeyedDispatch(NOTIFICATION_LISTENER_KEY), execution)
            .map_err(clean_failure)
    }

    fn notification_dismiss(
        &self,
        execution: &AdmittedExecution,
        identity: &AndroidNotificationIdentity,
        claim: &LocalExecutionClaim,
    ) -> Result<(), ExecutionFailure> {
        provider(execution, ProviderToken::NotificationListener, claim)?;
        runtime::bridge_notification_dismiss(
            &KeyedDispatch(NOTIFICATION_LISTENER_KEY),
            execution,
            identity,
        )
        .map_err(clean_failure)
    }

    fn notification_invoke(
        &self,
        execution: &AdmittedExecution,
        identity: &AndroidNotificationIdentity,
        action_index: u8,
        claim: &LocalExecutionClaim,
    ) -> Result<(), ExecutionFailure> {
        provider(execution, ProviderToken::NotificationListener, claim)?;
        runtime::bridge_notification_invoke(
            &KeyedDispatch(NOTIFICATION_LISTENER_KEY),
            execution,
            identity,
            action_index,
        )
        .map_err(clean_failure)
    }
}
