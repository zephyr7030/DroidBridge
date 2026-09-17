use crate::PortFuture;
use contract::{AutomationAction, AutomationCompatibleCall, ConditionSource, ScalarValue};
use domain::{DomainError, evaluate_condition};
use std::collections::BTreeMap;

/// Effects an AutomationExecution performs while the interpreter walks its immutable action tree
/// (S-AUTO-002). The interpreter owns only traversal and condition evaluation; every call, state
/// commit, wait and budget or cancellation check belongs to the effects owner.
pub trait AutomationEffects: Send {
    /// Rechecks the elapsed-time budget and cancellation before a visit and after an awaited
    /// operation.
    fn checkpoint(&mut self) -> Result<(), DomainError>;

    /// Invokes one Automation-compatible public action through the ordinary Contract path and
    /// awaits its terminal outcome.
    fn call<'a>(
        &'a mut self,
        call: &'a AutomationCompatibleCall,
    ) -> PortFuture<'a, Result<(), DomainError>>;

    /// Atomically creates or replaces one persistent state key of the owning Automation.
    fn set_state<'a>(
        &'a mut self,
        key: &'a str,
        value: &'a ScalarValue,
    ) -> PortFuture<'a, Result<(), DomainError>>;

    /// Waits `duration_ms`, bounded by the remaining budget and cancellable.
    fn delay<'a>(&'a mut self, duration_ms: u64) -> PortFuture<'a, Result<(), DomainError>>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct AutomationInterpreter;

impl AutomationInterpreter {
    /// Executes an already-validated action tree. `state` mirrors the owning Automation's
    /// persistent state and is updated only after each `set_state` commit succeeds.
    pub async fn execute<F: AutomationEffects>(
        &self,
        action: &AutomationAction,
        state: &mut BTreeMap<String, ScalarValue>,
        trigger: &BTreeMap<String, ScalarValue>,
        effects: &mut F,
    ) -> Result<(), DomainError> {
        visit(action, state, trigger, effects).await
    }
}

fn visit<'a, F: AutomationEffects>(
    action: &'a AutomationAction,
    state: &'a mut BTreeMap<String, ScalarValue>,
    trigger: &'a BTreeMap<String, ScalarValue>,
    effects: &'a mut F,
) -> PortFuture<'a, Result<(), DomainError>> {
    Box::pin(async move {
        effects.checkpoint()?;
        match action {
            AutomationAction::Call { call } => {
                effects.call(call).await?;
                effects.checkpoint()
            }
            AutomationAction::Sequence { children } => {
                for child in children {
                    visit(child, state, trigger, effects).await?;
                }
                Ok(())
            }
            AutomationAction::Conditional {
                condition,
                then,
                else_action,
            } => {
                let matched = match condition.source {
                    ConditionSource::State => evaluate_condition(condition, state),
                    ConditionSource::Trigger => evaluate_condition(condition, trigger),
                };
                if matched {
                    visit(then, state, trigger, effects).await
                } else if let Some(else_action) = else_action {
                    visit(else_action, state, trigger, effects).await
                } else {
                    Ok(())
                }
            }
            AutomationAction::Repeat {
                count,
                action,
                delay_ms,
            } => {
                for index in 0..*count {
                    visit(action, state, trigger, effects).await?;
                    if index + 1 < *count && *delay_ms > 0 {
                        effects.delay(*delay_ms).await?;
                        effects.checkpoint()?;
                    }
                }
                Ok(())
            }
            AutomationAction::Delay { duration_ms } => {
                effects.delay(*duration_ms).await?;
                effects.checkpoint()
            }
            AutomationAction::SetState { key, value } => {
                effects.set_state(key, value).await?;
                state.insert(key.clone(), value.clone());
                Ok(())
            }
        }
    })
}
