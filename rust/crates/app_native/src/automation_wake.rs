//! The APK host's Automation time-wake projection (S-LIFE-003): exactly one
//! `AlarmManager.setExactAndAllowWhileIdle(RTC_WAKEUP)` alarm owned by the App framework adapter,
//! armed at the earliest persisted due through the typed `AlarmSchedule`/`AlarmCancel` primitives.
//! It copies no schedule; the alarm and reconcile receivers only ask the scheduler to rescan.

use contract::{CapabilityState, ErrorCode, ExecutionClass};
use domain::DomainError;
use runtime::{
    AdmittedExecution, AndroidExecutionDispatch, AutomationWakeDue, AutomationWakeProjection,
    CapabilityPort, ExecutionPayload, ExecutorRecord, PortFuture, ProviderToken,
};
use tokio::sync::Notify;

pub(crate) struct ApkAlarmWake<D, C> {
    dispatch: D,
    capabilities: C,
    fired: Notify,
}

impl<D, C> ApkAlarmWake<D, C> {
    pub(crate) fn new(dispatch: D, capabilities: C) -> Self {
        Self {
            dispatch,
            capabilities,
            fired: Notify::new(),
        }
    }

    /// Delivers one exact-alarm or reconcile receiver wake; wakes before the scheduler waits
    /// coalesce into one rescan.
    pub(crate) fn fire(&self) {
        self.fired.notify_one();
    }
}

impl<D, C> AutomationWakeProjection for ApkAlarmWake<D, C>
where
    D: AndroidExecutionDispatch + Send + Sync,
    C: CapabilityPort + Send + Sync,
{
    fn arm(&self, due: Option<&AutomationWakeDue>) -> Result<bool, DomainError> {
        let capability = self.capabilities.current()?;
        if capability.grants.automation_exact_alarm.state != CapabilityState::Available
            || capability.context.app_execution_surface != CapabilityState::Available
            || capability.resolver_facts.generations.app_framework == 0
        {
            // The unavailable `automation.exact_alarm` grant or App surface is the explicit
            // loss; the persisted due stays unchanged and is applied when it returns.
            return Ok(false);
        }
        let execution = AdmittedExecution {
            execution_id: crate::new_uuid()?,
            task_id: None,
            executor: ExecutorRecord {
                host: capability.context.host,
                provider: ProviderToken::AppFramework,
                execution_class: ExecutionClass::AndroidFramework,
                capability_generation: capability.resolver_facts.generations.app_framework,
                fence: contract::Fence {
                    runtime_epoch: capability.fence.runtime_epoch,
                    host_generation: capability.fence.host_generation,
                    runtime_instance_id: capability.fence.runtime_instance_id,
                },
            },
            payload: ExecutionPayload::OpaqueOperation("automation.wake".to_owned()),
        };
        let (primitive, payload, expected) = match due {
            Some(due) => (
                "AlarmSchedule",
                serde_json::json!({ "due_unix_millis": due.unix_millis }),
                serde_json::json!({ "scheduled": true }),
            ),
            None => (
                "AlarmCancel",
                serde_json::json!({}),
                serde_json::json!({ "cancelled": true }),
            ),
        };
        let payload = serde_json::to_vec(&payload).map_err(|_| {
            DomainError::new(ErrorCode::InternalError, "alarm request encoding failed")
        })?;
        let result = match self.dispatch.dispatch(primitive, &payload, &execution) {
            Ok(result) => result,
            // The exact-alarm special access was revoked after the snapshot was taken.
            Err(error) if error.code == ErrorCode::CapabilityUnavailable => return Ok(false),
            Err(error) => return Err(error),
        };
        let reply: serde_json::Value = serde_json::from_slice(&result.payload)
            .map_err(|_| DomainError::new(ErrorCode::IoError, "alarm result is invalid"))?;
        if !result.descriptors.is_empty() || reply != expected {
            return Err(DomainError::new(
                ErrorCode::IoError,
                "alarm result is invalid",
            ));
        }
        Ok(true)
    }

    fn wait<'a>(&'a self) -> PortFuture<'a, Result<(), DomainError>> {
        Box::pin(async move {
            self.fired.notified().await;
            Ok(())
        })
    }
}
