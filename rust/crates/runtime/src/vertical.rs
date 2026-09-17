use contract::{
    Availability, CapabilityState, Compatibility, CompatibilityState, ComponentStatus, ContextCall,
    ContextDetail, ContextStatusCompact, ContextStatusFull, DeviceCompact, DeviceFull, ErrorCode,
    GrantFacts, IntegrationFact, MagiskComponent, PublicPayload, PublicRequest, RuntimeComponent,
    RuntimeHost, RuntimeReadiness, RuntimeStatus, UuidV4, VersionFact,
};
use domain::{
    AdmissionFence, CapabilityContext, DomainError, ProviderGenerations, ResolverFacts,
    derive_capabilities,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

pub const UI_ENVELOPE_LIMIT_BYTES: usize = 262_144;
/// The pinned Shizuku API the APK integrates (R-CONTEXT-005).
const SHIZUKU_INTEGRATION_VERSION: &str = "13.1.5";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerticalEnvironment {
    pub sdk_int: u32,
    pub abi: String,
    pub timezone: String,
    pub manufacturer: String,
    pub model: String,
    pub device: String,
    pub build_fingerprint: String,
    pub version_name: String,
    pub version_code: u64,
    pub runtime_epoch: UuidV4,
    pub host_generation: u64,
}

#[derive(Clone, Debug, PartialEq)]
struct Registration {
    availability: Availability,
    source_generation: u64,
    has_executor: bool,
}

pub struct ApkRuntimeVertical {
    environment: VerticalEnvironment,
    host: RuntimeHost,
    registrations: Arc<Mutex<BTreeMap<String, Registration>>>,
    condition: Arc<Mutex<(RuntimeReadiness, Option<String>)>>,
    app_execution_surface: Arc<Mutex<CapabilityState>>,
}

impl ApkRuntimeVertical {
    pub fn new(environment: VerticalEnvironment) -> Result<Self, DomainError> {
        Self::new_for_host(environment, RuntimeHost::ApkRuntime)
    }

    pub fn new_for_host(
        environment: VerticalEnvironment,
        host: RuntimeHost,
    ) -> Result<Self, DomainError> {
        if !(33..=37).contains(&environment.sdk_int)
            || environment.host_generation == 0
            || environment.abi.is_empty()
            || environment.timezone.is_empty()
        {
            return Err(DomainError::invalid("invalid APK Runtime environment"));
        }
        let mut registrations = BTreeMap::new();
        for key in contract::GRANT_KEYS {
            registrations.insert(
                (*key).to_owned(),
                Registration {
                    availability: unavailable_or_unknown(key),
                    source_generation: 0,
                    has_executor: false,
                },
            );
        }
        Ok(Self {
            environment,
            host,
            registrations: Arc::new(Mutex::new(registrations)),
            condition: Arc::new(Mutex::new((RuntimeReadiness::Ready, None))),
            app_execution_surface: Arc::new(Mutex::new(if host == RuntimeHost::ApkRuntime {
                CapabilityState::Available
            } else {
                CapabilityState::Unavailable
            })),
        })
    }

    pub fn set_app_execution_surface(&self, state: CapabilityState) -> Result<(), DomainError> {
        *self.app_execution_surface.lock().map_err(|_| {
            DomainError::new(
                ErrorCode::InternalError,
                "App execution-surface lock failed",
            )
        })? = state;
        Ok(())
    }

    pub fn set_unavailable(&self, reason: &str) -> Result<(), DomainError> {
        if reason.is_empty() {
            return Err(DomainError::invalid("Runtime reason is empty"));
        }
        *self.condition.lock().map_err(|_| {
            DomainError::new(ErrorCode::InternalError, "Runtime condition lock failed")
        })? = (RuntimeReadiness::Unavailable, Some(reason.to_owned()));
        Ok(())
    }

    pub fn register_capability(
        &self,
        key: &str,
        availability: Availability,
        source_generation: u64,
        has_executor: bool,
    ) -> Result<bool, DomainError> {
        if source_generation == 0 || !contract::GRANT_KEYS.contains(&key) {
            return Err(DomainError::invalid("invalid capability registration"));
        }
        if availability.state != CapabilityState::Available && has_executor {
            return Err(DomainError::invalid(
                "an unavailable capability cannot own an executor",
            ));
        }
        let mut values = self
            .registrations
            .lock()
            .map_err(|_| DomainError::new(ErrorCode::InternalError, "registration lock failed"))?;
        let current = values
            .get(key)
            .ok_or_else(|| DomainError::invalid("unknown capability registration"))?;
        if source_generation < current.source_generation {
            return Ok(false);
        }
        if source_generation == current.source_generation {
            return Ok(current.availability == availability && current.has_executor == has_executor);
        }
        values.insert(
            key.to_owned(),
            Registration {
                availability,
                source_generation,
                has_executor,
            },
        );
        Ok(true)
    }

    pub fn withdraw_capabilities(&self, keys: &[&str], reason: &str) -> Result<(), DomainError> {
        if reason.is_empty() || keys.iter().any(|key| !contract::GRANT_KEYS.contains(key)) {
            return Err(DomainError::invalid("invalid capability withdrawal"));
        }
        let mut values = self
            .registrations
            .lock()
            .map_err(|_| DomainError::new(ErrorCode::InternalError, "registration lock failed"))?;
        for key in keys {
            values.insert(
                (*key).to_owned(),
                Registration {
                    availability: Availability {
                        state: CapabilityState::Unavailable,
                        reason: Some(reason.to_owned()),
                    },
                    source_generation: 0,
                    has_executor: false,
                },
            );
        }
        Ok(())
    }

    pub fn dispatch_installed(&self, request: PublicRequest) -> Result<Value, DomainError> {
        match request.payload {
            PublicPayload::Context {
                call: ContextCall::Status(input),
            } => self.context_status(input.detail),
            _ => Err(DomainError::new(
                ErrorCode::Unsupported,
                "the I5 APK vertical does not own this action",
            )),
        }
    }

    pub fn capability_port(&self, runtime_instance_id: UuidV4) -> ApkCapabilityPort {
        ApkCapabilityPort {
            environment: self.environment.clone(),
            host: self.host,
            runtime_instance_id,
            registrations: Arc::clone(&self.registrations),
            condition: Arc::clone(&self.condition),
            app_execution_surface: Arc::clone(&self.app_execution_surface),
        }
    }

    fn context_status(&self, detail: ContextDetail) -> Result<Value, DomainError> {
        let grants = self.grants()?;
        let (readiness, reason) = self
            .condition
            .lock()
            .map_err(|_| {
                DomainError::new(ErrorCode::InternalError, "Runtime condition lock failed")
            })?
            .clone();
        let capabilities = derive_capabilities(
            &grants,
            CapabilityContext {
                sdk_int: self.environment.sdk_int,
                host: self.host,
                readiness,
                app_execution_surface: *self.app_execution_surface.lock().map_err(|_| {
                    DomainError::new(
                        ErrorCode::InternalError,
                        "App execution-surface lock failed",
                    )
                })?,
            },
        )?;
        let runtime = RuntimeStatus {
            host: self.host,
            host_generation: self.environment.host_generation,
            readiness,
            reason,
        };
        let value = match detail {
            ContextDetail::Compact => serde_json::to_value(ContextStatusCompact {
                device: DeviceCompact {
                    sdk_int: self.environment.sdk_int,
                    abi: self.environment.abi.clone(),
                    timezone: self.environment.timezone.clone(),
                },
                runtime,
                capabilities,
            }),
            ContextDetail::Full => {
                // Component state is the observed integration fact the owning adapter registered;
                // an unobserved optional version is omitted rather than guessed (S-CONTEXT-001).
                let components = ComponentStatus {
                    apk: VersionFact {
                        version_name: self.environment.version_name.clone(),
                        version_code: self.environment.version_code,
                    },
                    runtime_host: RuntimeComponent {
                        host: self.host,
                        component_version: self.environment.version_name.clone(),
                        protocol_version: contract::PROTOCOL_VERSION,
                        store_schema_version: contract::STORE_SCHEMA_VERSION,
                    },
                    shizuku: IntegrationFact {
                        integration_version: SHIZUKU_INTEGRATION_VERSION.to_owned(),
                        manager_version: None,
                        state: grants.shizuku_shell.state,
                        reason: grants.shizuku_shell.reason.clone(),
                    },
                    magisk: MagiskComponent {
                        module_version: None,
                        daemon_version: (self.host == RuntimeHost::MagiskBackend)
                            .then(|| self.environment.version_name.clone()),
                        state: grants.magisk_module.state,
                        reason: grants.magisk_module.reason.clone(),
                    },
                };
                serde_json::to_value(ContextStatusFull {
                    device: DeviceFull {
                        sdk_int: self.environment.sdk_int,
                        abi: self.environment.abi.clone(),
                        timezone: self.environment.timezone.clone(),
                        manufacturer: self.environment.manufacturer.clone(),
                        model: self.environment.model.clone(),
                        device: self.environment.device.clone(),
                        build_fingerprint: self.environment.build_fingerprint.clone(),
                    },
                    runtime,
                    capabilities,
                    grants,
                    components,
                    // Directly established by the answering host: it decoded this request at
                    // protocol v1 and serves it from a store it loaded at the current schema.
                    compatibility: Compatibility {
                        protocol: CompatibilityState::Compatible,
                        store_schema: CompatibilityState::Compatible,
                    },
                })
            }
        }
        .map_err(|_| DomainError::new(ErrorCode::InternalError, "context serialization failed"))?;
        Ok(value)
    }

    fn grants(&self) -> Result<GrantFacts, DomainError> {
        let values = self
            .registrations
            .lock()
            .map_err(|_| DomainError::new(ErrorCode::InternalError, "registration lock failed"))?;
        grant_facts(&values)
    }
}

fn grant_facts(values: &BTreeMap<String, Registration>) -> Result<GrantFacts, DomainError> {
    let get = |key: &str| {
        values
            .get(key)
            .map(|value| value.availability.clone())
            .ok_or_else(|| DomainError::new(ErrorCode::InternalError, "missing grant fact"))
    };
    Ok(GrantFacts {
        android_local_network: get("android.local_network")?,
        android_notifications: get("android.notifications")?,
        android_notification_listener: get("android.notification_listener")?,
        automation_exact_alarm: get("automation.exact_alarm")?,
        visual_accessibility: get("visual.accessibility")?,
        visual_media_projection_session: get("visual.media_projection_session")?,
        shizuku_shell: get("shizuku.shell")?,
        magisk_module: get("magisk.module")?,
        magisk_root: get("magisk.root")?,
        magisk_framework: get("magisk.framework")?,
        magisk_launch: get("magisk.launch")?,
        magisk_clipboard: get("magisk.clipboard")?,
        magisk_notifications: get("magisk.notifications")?,
        magisk_wake_alarm: get("magisk.wake_alarm")?,
        execution_app_guard: get("execution.app_guard")?,
        execution_shell_guard: get("execution.shell_guard")?,
        execution_root_guard: get("execution.root_guard")?,
    })
}

#[derive(Clone)]
pub struct ApkCapabilityPort {
    environment: VerticalEnvironment,
    host: RuntimeHost,
    runtime_instance_id: UuidV4,
    registrations: Arc<Mutex<BTreeMap<String, Registration>>>,
    condition: Arc<Mutex<(RuntimeReadiness, Option<String>)>>,
    app_execution_surface: Arc<Mutex<CapabilityState>>,
}

impl ApkCapabilityPort {
    pub fn withdraw_readiness(&self) -> Result<(), DomainError> {
        *self.condition.lock().map_err(|_| {
            DomainError::new(ErrorCode::InternalError, "Runtime condition lock failed")
        })? = (
            RuntimeReadiness::Unavailable,
            Some("CLEANUP_UNVERIFIED".to_owned()),
        );
        Ok(())
    }
}

impl crate::CapabilityPort for ApkCapabilityPort {
    fn current(&self) -> Result<crate::CapabilitySnapshot, DomainError> {
        let values = self
            .registrations
            .lock()
            .map_err(|_| DomainError::new(ErrorCode::InternalError, "registration lock failed"))?;
        let state = |key: &str| {
            values
                .get(key)
                .map(|value| value.availability.state)
                .ok_or_else(|| DomainError::new(ErrorCode::InternalError, "missing grant fact"))
        };
        let generation = |key: &str| {
            values
                .get(key)
                .map(|value| value.source_generation.max(1))
                .ok_or_else(|| DomainError::new(ErrorCode::InternalError, "missing grant fact"))
        };
        let readiness = self
            .condition
            .lock()
            .map_err(|_| {
                DomainError::new(ErrorCode::InternalError, "Runtime condition lock failed")
            })?
            .0;
        // The Android framework provider is the authenticated APK surface itself, so
        // this host's framework fact is exactly that surface's reachability: always
        // present when the APK Runtime is host, and present on the Magisk host only
        // while an authenticated companion is connected (S-AUTH-003).
        let app_execution_surface = *self.app_execution_surface.lock().map_err(|_| {
            DomainError::new(
                ErrorCode::InternalError,
                "App execution-surface lock failed",
            )
        })?;
        Ok(crate::CapabilitySnapshot {
            grants: grant_facts(&values)?,
            context: CapabilityContext {
                sdk_int: self.environment.sdk_int,
                host: self.host,
                readiness,
                app_execution_surface,
            },
            resolver_facts: ResolverFacts {
                app_native: state("execution.app_guard")?,
                app_framework: app_execution_surface,
                shizuku: state("shizuku.shell")?,
                magisk_native: state("magisk.root")?,
                magisk_framework: state("magisk.framework")?,
                magisk_launch: state("magisk.launch")?,
                magisk_clipboard: state("magisk.clipboard")?,
                magisk_notifications: state("magisk.notifications")?,
                accessibility: state("visual.accessibility")?,
                media_projection: state("visual.media_projection_session")?,
                notification_listener: state("android.notification_listener")?,
                generations: ProviderGenerations {
                    app_native: generation("execution.app_guard")?,
                    app_framework: self.environment.host_generation,
                    shizuku: generation("shizuku.shell")?,
                    magisk_native: generation("magisk.root")?,
                    magisk_framework: generation("magisk.framework")?,
                    accessibility: generation("visual.accessibility")?,
                    media_projection: generation("visual.media_projection_session")?,
                    notification_listener: generation("android.notification_listener")?,
                },
            },
            fence: AdmissionFence {
                runtime_epoch: self.environment.runtime_epoch.clone(),
                host_generation: self.environment.host_generation,
                runtime_instance_id: self.runtime_instance_id.clone(),
            },
        })
    }
}

fn unavailable_or_unknown(key: &str) -> Availability {
    if key == "visual.media_projection_session" {
        Availability {
            state: CapabilityState::Unavailable,
            reason: Some("USER_CONSENT_REQUIRED".to_owned()),
        }
    } else {
        Availability {
            state: CapabilityState::Unknown,
            reason: Some("ADAPTER_NOT_READY".to_owned()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CapabilityPort;

    fn environment() -> VerticalEnvironment {
        VerticalEnvironment {
            sdk_int: 37,
            abi: "x86_64".to_owned(),
            timezone: "UTC".to_owned(),
            manufacturer: "test".to_owned(),
            model: "test".to_owned(),
            device: "test".to_owned(),
            build_fingerprint: "test".to_owned(),
            version_name: "0.1.0".to_owned(),
            version_code: 1000,
            runtime_epoch: UuidV4::parse("123e4567-e89b-42d3-a456-426614174000").unwrap(),
            host_generation: 1,
        }
    }

    #[test]
    fn i5_g03_stale_capability_registration_cannot_replace_newer_truth() {
        let runtime = ApkRuntimeVertical::new(environment()).unwrap();
        let available = Availability {
            state: CapabilityState::Available,
            reason: None,
        };
        let unavailable = Availability {
            state: CapabilityState::Unavailable,
            reason: Some("DENIED".to_owned()),
        };
        assert!(
            runtime
                .register_capability("android.notifications", available, 2, false)
                .unwrap()
        );
        assert!(
            !runtime
                .register_capability("android.notifications", unavailable, 1, false)
                .unwrap()
        );
        assert_eq!(
            runtime.grants().unwrap().android_notifications.state,
            CapabilityState::Available
        );
    }

    #[test]
    fn i5_g01_core_capability_port_shares_the_authoritative_registration_projection() {
        let runtime = ApkRuntimeVertical::new(environment()).unwrap();
        let runtime_instance = UuidV4::parse("123e4567-e89b-42d3-a456-426614174001").unwrap();
        let available = Availability {
            state: CapabilityState::Available,
            reason: None,
        };
        runtime
            .register_capability("execution.app_guard", available, 4, true)
            .unwrap();

        let snapshot = runtime
            .capability_port(runtime_instance.clone())
            .current()
            .unwrap();
        assert_eq!(snapshot.fence.runtime_instance_id, runtime_instance);
        assert_eq!(
            snapshot.resolver_facts.app_native,
            CapabilityState::Available
        );
        assert_eq!(snapshot.resolver_facts.generations.app_native, 4);
    }

    #[test]
    fn i7_g05_companion_fact_withdrawal_does_not_touch_magisk_facts() {
        let runtime =
            ApkRuntimeVertical::new_for_host(environment(), RuntimeHost::MagiskBackend).unwrap();
        let available = Availability {
            state: CapabilityState::Available,
            reason: None,
        };
        runtime
            .register_capability("shizuku.shell", available.clone(), 4, true)
            .unwrap();
        runtime
            .register_capability("magisk.root", available, 5, true)
            .unwrap();

        runtime
            .withdraw_capabilities(&["shizuku.shell"], "COMPANION_UNAVAILABLE")
            .unwrap();

        let grants = runtime.grants().unwrap();
        assert_eq!(grants.shizuku_shell.state, CapabilityState::Unavailable);
        assert_eq!(
            grants.shizuku_shell.reason.as_deref(),
            Some("COMPANION_UNAVAILABLE")
        );
        assert_eq!(grants.magisk_root.state, CapabilityState::Available);
    }

    #[test]
    fn i5_g07_quarantine_remains_queryable_but_blocks_operations() {
        let runtime = ApkRuntimeVertical::new(environment()).unwrap();
        runtime.set_unavailable("CLEANUP_UNVERIFIED").unwrap();
        let context = serde_json::json!({
            "protocol_version": 1,
            "request_id": "123e4567-e89b-42d3-a456-426614174003",
            "payload": {"tool": "context", "action": "status", "input": {"detail": "full"}}
        });
        let request: PublicRequest = serde_json::from_value(context).unwrap();
        let response = runtime.dispatch_installed(request).unwrap();

        assert_eq!(response["runtime"]["readiness"], "unavailable");
        assert_eq!(response["runtime"]["reason"], "CLEANUP_UNVERIFIED");
    }
}
