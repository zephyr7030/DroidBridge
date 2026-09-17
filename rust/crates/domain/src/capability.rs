use crate::DomainError;
use contract::{
    Availability, CapabilityState, EffectiveCapabilities, GrantFacts, RuntimeHost, RuntimeReadiness,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CapabilityContext {
    pub sdk_int: u32,
    pub host: RuntimeHost,
    pub readiness: RuntimeReadiness,
    pub app_execution_surface: CapabilityState,
}

pub fn any_sufficient(states: &[CapabilityState]) -> CapabilityState {
    if states.contains(&CapabilityState::Available) {
        CapabilityState::Available
    } else if states
        .iter()
        .all(|state| *state == CapabilityState::Unavailable)
    {
        CapabilityState::Unavailable
    } else {
        CapabilityState::Unknown
    }
}

pub fn all_required(states: &[CapabilityState]) -> CapabilityState {
    if states.contains(&CapabilityState::Unavailable) {
        CapabilityState::Unavailable
    } else if states
        .iter()
        .all(|state| *state == CapabilityState::Available)
    {
        CapabilityState::Available
    } else {
        CapabilityState::Unknown
    }
}

fn base(readiness: RuntimeReadiness) -> CapabilityState {
    match readiness {
        RuntimeReadiness::Ready => CapabilityState::Available,
        RuntimeReadiness::Unavailable => CapabilityState::Unavailable,
        RuntimeReadiness::Initializing => CapabilityState::Unknown,
    }
}

fn output(state: CapabilityState) -> Availability {
    Availability {
        state,
        reason: None,
    }
}

pub fn derive_capabilities(
    grants: &GrantFacts,
    context: CapabilityContext,
) -> Result<EffectiveCapabilities, DomainError> {
    if !(33..=37).contains(&context.sdk_int) {
        return Err(DomainError::invalid(
            "sdk_int is outside the supported range",
        ));
    }
    let runtime = base(context.readiness);
    let gated = |state| output(all_required(&[runtime, state]));
    let any = |states: &[CapabilityState]| any_sufficient(states);
    let privileged_path = any(&[grants.magisk_root.state, grants.shizuku_shell.state]);
    let command_app = all_required(&[
        context.app_execution_surface,
        grants.execution_app_guard.state,
    ]);
    let command_shell = all_required(&[
        grants.shizuku_shell.state,
        grants.execution_shell_guard.state,
    ]);
    let command_root = all_required(&[grants.magisk_root.state, grants.execution_root_guard.state]);
    let network_local = if context.sdk_int <= 36 {
        CapabilityState::Available
    } else {
        any(&[grants.magisk_root.state, grants.android_local_network.state])
    };
    let visual_base = any(&[
        grants.magisk_root.state,
        grants.shizuku_shell.state,
        grants.visual_accessibility.state,
    ]);
    let visual_image = any(&[
        grants.magisk_root.state,
        grants.shizuku_shell.state,
        grants.visual_accessibility.state,
        grants.visual_media_projection_session.state,
    ]);
    let persistent_time = match context.host {
        RuntimeHost::MagiskBackend => grants.magisk_wake_alarm.state,
        RuntimeHost::ApkRuntime => grants.automation_exact_alarm.state,
    };
    let notifications = any(&[
        grants.magisk_notifications.state,
        grants.android_notification_listener.state,
    ]);
    Ok(EffectiveCapabilities {
        filesystem_privileged_path: gated(privileged_path),
        command_app: gated(command_app),
        command_shell: gated(command_shell),
        command_root: gated(command_root),
        network_inspect: output(runtime),
        network_internet: output(runtime),
        network_local: gated(network_local),
        network_capture: gated(grants.magisk_root.state),
        network_inject: gated(grants.magisk_root.state),
        visual_image: gated(visual_image),
        visual_hierarchy: gated(visual_base),
        visual_coordinate_input: gated(visual_base),
        visual_key_input: gated(any(&[grants.magisk_root.state, grants.shizuku_shell.state])),
        visual_text_input: gated(visual_base),
        automation_persistent_time: gated(persistent_time),
        android_notification_access: gated(notifications),
    })
}
