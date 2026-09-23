use crate::DomainError;
use contract::{
    CapabilityState, ErrorCode, ExecutionClass, FileTargetType, RunAs, RuntimeHost, UuidV4,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Provider {
    AppNative,
    AppFramework,
    Shizuku,
    MagiskNative,
    MagiskFramework,
    Accessibility,
    MediaProjection,
    NotificationListener,
}

impl Provider {
    pub const fn execution_class(self) -> ExecutionClass {
        match self {
            Self::AppNative => ExecutionClass::App,
            Self::AppFramework
            | Self::Accessibility
            | Self::MediaProjection
            | Self::NotificationListener => ExecutionClass::AndroidFramework,
            Self::Shizuku => ExecutionClass::Shizuku,
            Self::MagiskNative => ExecutionClass::Magisk,
            Self::MagiskFramework => ExecutionClass::AndroidFramework,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdmissionFence {
    pub runtime_epoch: UuidV4,
    pub host_generation: u64,
    pub runtime_instance_id: UuidV4,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdmittedExecutor {
    host: RuntimeHost,
    provider: Provider,
    execution_class: ExecutionClass,
    capability_generation: u64,
    fence: AdmissionFence,
}

impl AdmittedExecutor {
    pub const fn host(&self) -> RuntimeHost {
        self.host
    }

    pub const fn provider(&self) -> Provider {
        self.provider
    }

    pub const fn execution_class(&self) -> ExecutionClass {
        self.execution_class
    }

    pub const fn capability_generation(&self) -> u64 {
        self.capability_generation
    }

    pub const fn fence(&self) -> &AdmissionFence {
        &self.fence
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Preflight {
    Positive,
    Negative,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResolverFacts {
    pub app_native: CapabilityState,
    pub app_framework: CapabilityState,
    pub shizuku: CapabilityState,
    pub magisk_native: CapabilityState,
    pub magisk_framework: CapabilityState,
    pub magisk_launch: CapabilityState,
    pub magisk_clipboard: CapabilityState,
    pub magisk_notifications: CapabilityState,
    pub accessibility: CapabilityState,
    pub media_projection: CapabilityState,
    pub notification_listener: CapabilityState,
    pub generations: ProviderGenerations,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProviderGenerations {
    pub app_native: u64,
    pub app_framework: u64,
    pub shizuku: u64,
    pub magisk_native: u64,
    pub magisk_framework: u64,
    pub accessibility: u64,
    pub media_projection: u64,
    pub notification_listener: u64,
}

impl ProviderGenerations {
    const fn for_provider(self, provider: Provider) -> u64 {
        match provider {
            Provider::AppNative => self.app_native,
            Provider::AppFramework => self.app_framework,
            Provider::Shizuku => self.shizuku,
            Provider::MagiskNative => self.magisk_native,
            Provider::MagiskFramework => self.magisk_framework,
            Provider::Accessibility => self.accessibility,
            Provider::MediaProjection => self.media_projection,
            Provider::NotificationListener => self.notification_listener,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NetworkRoute {
    InspectOrDiagnose,
    ReadOnlyRouteSupplement,
    Capture,
    Inject,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FilesystemRoute {
    InspectOrRead,
    Mutation,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VisualRoute {
    Display,
    Transform,
    Hierarchy,
    Image,
    CoordinateInput,
    KeyInput,
    FocusedText,
    AccessibilityNode,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PackageInspectFact {
    ExactSuccess,
    VisibilityOrAbsent,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AndroidRoute {
    PackageInspect(PackageInspectFact),
    PackageList,
    PackageForceStop,
    LaunchOrIntent,
    Clipboard,
    Notification,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExecutorRequest {
    Command(RunAs),
    Filesystem {
        route: FilesystemRoute,
        target_type: FileTargetType,
        app_preflight: Preflight,
        shizuku_preflight: Preflight,
    },
    Network(NetworkRoute),
    Visual(VisualRoute),
    Android(AndroidRoute),
}

fn available(state: CapabilityState) -> bool {
    state == CapabilityState::Available
}

fn choose(
    host: RuntimeHost,
    fence: AdmissionFence,
    candidates: &[(CapabilityState, Provider)],
    generations: ProviderGenerations,
) -> Result<AdmittedExecutor, DomainError> {
    let provider = candidates
        .iter()
        .find_map(|(state, provider)| available(*state).then_some(*provider))
        .ok_or_else(|| {
            DomainError::new(
                ErrorCode::CapabilityUnavailable,
                "no authoritative provider is available",
            )
        })?;
    Ok(AdmittedExecutor {
        host,
        provider,
        execution_class: provider.execution_class(),
        capability_generation: generations.for_provider(provider),
        fence,
    })
}

pub fn resolve_executor(
    host: RuntimeHost,
    fence: AdmissionFence,
    facts: ResolverFacts,
    request: ExecutorRequest,
) -> Result<AdmittedExecutor, DomainError> {
    let candidates: Vec<(CapabilityState, Provider)> = match (host, request) {
        (RuntimeHost::ApkRuntime, ExecutorRequest::Command(RunAs::App)) => {
            vec![(facts.app_native, Provider::AppNative)]
        }
        (RuntimeHost::ApkRuntime, ExecutorRequest::Command(RunAs::Shell)) => {
            vec![(facts.shizuku, Provider::Shizuku)]
        }
        (RuntimeHost::ApkRuntime, ExecutorRequest::Command(RunAs::Root)) => {
            return Err(DomainError::new(
                ErrorCode::RunAsUnavailable,
                "root identity is unavailable on the APK surface",
            ));
        }
        (RuntimeHost::MagiskBackend, ExecutorRequest::Command(RunAs::Root)) => {
            vec![(facts.magisk_native, Provider::MagiskNative)]
        }
        (RuntimeHost::MagiskBackend, ExecutorRequest::Command(RunAs::App)) => {
            vec![(facts.app_native, Provider::AppNative)]
        }
        (RuntimeHost::MagiskBackend, ExecutorRequest::Command(RunAs::Shell)) => {
            vec![(facts.shizuku, Provider::Shizuku)]
        }
        (
            RuntimeHost::MagiskBackend,
            ExecutorRequest::Filesystem {
                target_type: FileTargetType::Path,
                ..
            },
        ) => vec![(facts.magisk_native, Provider::MagiskNative)],
        (
            _,
            ExecutorRequest::Filesystem {
                route: FilesystemRoute::Mutation,
                target_type: FileTargetType::ContentUri,
                ..
            },
        ) => {
            return Err(DomainError::new(
                ErrorCode::Unsupported,
                "content URI mutation is unsupported in protocol v1",
            ));
        }
        (
            RuntimeHost::MagiskBackend,
            ExecutorRequest::Filesystem {
                route: FilesystemRoute::InspectOrRead,
                target_type: FileTargetType::ContentUri,
                ..
            },
        ) => vec![(facts.app_framework, Provider::AppFramework)],
        (
            RuntimeHost::ApkRuntime,
            ExecutorRequest::Filesystem {
                route: FilesystemRoute::InspectOrRead,
                target_type: FileTargetType::ContentUri,
                ..
            },
        ) => vec![(facts.app_framework, Provider::AppFramework)],
        (
            RuntimeHost::ApkRuntime,
            ExecutorRequest::Filesystem {
                route: _,
                target_type: FileTargetType::Path,
                app_preflight,
                shizuku_preflight,
            },
        ) => vec![
            (
                if app_preflight == Preflight::Positive {
                    facts.app_native
                } else {
                    CapabilityState::Unavailable
                },
                Provider::AppNative,
            ),
            (
                if shizuku_preflight == Preflight::Positive {
                    facts.shizuku
                } else {
                    CapabilityState::Unavailable
                },
                Provider::Shizuku,
            ),
        ],
        (RuntimeHost::MagiskBackend, ExecutorRequest::Network(_)) => {
            vec![(facts.magisk_native, Provider::MagiskNative)]
        }
        (RuntimeHost::ApkRuntime, ExecutorRequest::Network(NetworkRoute::InspectOrDiagnose)) => {
            vec![(facts.app_native, Provider::AppNative)]
        }
        (
            RuntimeHost::ApkRuntime,
            ExecutorRequest::Network(NetworkRoute::ReadOnlyRouteSupplement),
        ) => vec![(facts.shizuku, Provider::Shizuku)],
        (
            RuntimeHost::ApkRuntime,
            ExecutorRequest::Network(NetworkRoute::Capture | NetworkRoute::Inject),
        ) => {
            return Err(DomainError::new(
                ErrorCode::CapabilityUnavailable,
                "raw network operations require the Magisk host",
            ));
        }
        (RuntimeHost::MagiskBackend, ExecutorRequest::Visual(VisualRoute::AccessibilityNode)) => {
            vec![(facts.accessibility, Provider::Accessibility)]
        }
        (RuntimeHost::MagiskBackend, ExecutorRequest::Visual(VisualRoute::Transform)) => {
            vec![(facts.app_framework, Provider::AppFramework)]
        }
        (
            RuntimeHost::MagiskBackend,
            ExecutorRequest::Visual(VisualRoute::Hierarchy | VisualRoute::CoordinateInput),
        ) => vec![
            (facts.accessibility, Provider::Accessibility),
            (facts.magisk_native, Provider::MagiskNative),
        ],
        (RuntimeHost::MagiskBackend, ExecutorRequest::Visual(VisualRoute::FocusedText)) => vec![
            (facts.accessibility, Provider::Accessibility),
            (facts.magisk_native, Provider::MagiskNative),
        ],
        (RuntimeHost::MagiskBackend, ExecutorRequest::Visual(_)) => {
            vec![(facts.magisk_native, Provider::MagiskNative)]
        }
        (
            RuntimeHost::ApkRuntime,
            ExecutorRequest::Visual(VisualRoute::Display | VisualRoute::Transform),
        ) => {
            vec![(facts.app_framework, Provider::AppFramework)]
        }
        (RuntimeHost::ApkRuntime, ExecutorRequest::Visual(VisualRoute::Hierarchy)) => vec![
            (facts.accessibility, Provider::Accessibility),
            (facts.shizuku, Provider::Shizuku),
        ],
        (RuntimeHost::ApkRuntime, ExecutorRequest::Visual(VisualRoute::Image)) => vec![
            (facts.shizuku, Provider::Shizuku),
            (facts.accessibility, Provider::Accessibility),
            (facts.media_projection, Provider::MediaProjection),
        ],
        (RuntimeHost::ApkRuntime, ExecutorRequest::Visual(VisualRoute::CoordinateInput)) => vec![
            (facts.accessibility, Provider::Accessibility),
            (facts.shizuku, Provider::Shizuku),
        ],
        (RuntimeHost::ApkRuntime, ExecutorRequest::Visual(VisualRoute::FocusedText)) => {
            vec![(facts.accessibility, Provider::Accessibility)]
        }
        (RuntimeHost::ApkRuntime, ExecutorRequest::Visual(VisualRoute::KeyInput)) => {
            vec![(facts.shizuku, Provider::Shizuku)]
        }
        (RuntimeHost::ApkRuntime, ExecutorRequest::Visual(VisualRoute::AccessibilityNode)) => {
            vec![(facts.accessibility, Provider::Accessibility)]
        }
        (
            RuntimeHost::MagiskBackend,
            ExecutorRequest::Android(
                AndroidRoute::PackageInspect(_)
                | AndroidRoute::PackageList
                | AndroidRoute::PackageForceStop,
            ),
        ) => vec![(facts.magisk_native, Provider::MagiskNative)],
        (RuntimeHost::MagiskBackend, ExecutorRequest::Android(AndroidRoute::LaunchOrIntent)) => {
            vec![
                (facts.magisk_launch, Provider::MagiskFramework),
                (facts.app_framework, Provider::AppFramework),
            ]
        }
        (RuntimeHost::MagiskBackend, ExecutorRequest::Android(AndroidRoute::Clipboard)) => vec![
            (facts.magisk_clipboard, Provider::MagiskFramework),
            (facts.app_framework, Provider::AppFramework),
        ],
        (RuntimeHost::MagiskBackend, ExecutorRequest::Android(AndroidRoute::Notification)) => vec![
            (facts.magisk_notifications, Provider::MagiskFramework),
            (facts.notification_listener, Provider::NotificationListener),
        ],
        (
            RuntimeHost::ApkRuntime,
            ExecutorRequest::Android(AndroidRoute::PackageInspect(
                PackageInspectFact::ExactSuccess,
            )),
        ) => vec![(facts.app_framework, Provider::AppFramework)],
        (
            RuntimeHost::ApkRuntime,
            ExecutorRequest::Android(AndroidRoute::PackageInspect(
                PackageInspectFact::VisibilityOrAbsent,
            )),
        ) => vec![(facts.shizuku, Provider::Shizuku)],
        (
            RuntimeHost::ApkRuntime,
            ExecutorRequest::Android(AndroidRoute::PackageInspect(PackageInspectFact::Unknown)),
        ) => vec![],
        (
            RuntimeHost::ApkRuntime,
            ExecutorRequest::Android(AndroidRoute::PackageList | AndroidRoute::PackageForceStop),
        ) => vec![(facts.shizuku, Provider::Shizuku)],
        (
            RuntimeHost::ApkRuntime,
            ExecutorRequest::Android(AndroidRoute::LaunchOrIntent | AndroidRoute::Clipboard),
        ) => vec![(facts.app_framework, Provider::AppFramework)],
        (RuntimeHost::ApkRuntime, ExecutorRequest::Android(AndroidRoute::Notification)) => {
            vec![(facts.notification_listener, Provider::NotificationListener)]
        }
    };
    choose(host, fence, &candidates, facts.generations)
}
