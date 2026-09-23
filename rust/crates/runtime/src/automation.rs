use crate::PortFuture;
use contract::{
    AutomationAction, AutomationCompatibleCall, ConditionSource, ErrorCode, ScalarValue,
};
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
    /// awaits its terminal outcome. An error is the failure of this Call.
    fn call<'a>(
        &'a mut self,
        call: &'a AutomationCompatibleCall,
    ) -> PortFuture<'a, Result<CallOutcome, DomainError>>;

    /// Atomically creates or replaces one persistent state key of the owning Automation.
    fn set_state<'a>(
        &'a mut self,
        key: &'a str,
        value: &'a ScalarValue,
    ) -> PortFuture<'a, Result<(), DomainError>>;

    /// Waits `duration_ms`, bounded by the remaining budget and cancellable.
    fn delay<'a>(&'a mut self, duration_ms: u64) -> PortFuture<'a, Result<(), DomainError>>;
}

/// What a Call that answered leaves for a later `result` condition.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CallOutcome {
    /// Set when the Call answered but did not succeed, such as a command that exited non-zero.
    pub failure: Option<ErrorCode>,
    pub exit_code: Option<i32>,
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
        let mut result = BTreeMap::new();
        visit(action, state, trigger, &mut result, effects).await
    }
}

fn visit<'a, F: AutomationEffects>(
    action: &'a AutomationAction,
    state: &'a mut BTreeMap<String, ScalarValue>,
    trigger: &'a BTreeMap<String, ScalarValue>,
    result: &'a mut BTreeMap<String, ScalarValue>,
    effects: &'a mut F,
) -> PortFuture<'a, Result<(), DomainError>> {
    Box::pin(async move {
        effects.checkpoint()?;
        match action {
            AutomationAction::Call { call, on_failure } => {
                let (failure, exit_code) = match effects.call(call).await {
                    Ok(outcome) => (
                        outcome
                            .failure
                            .map(|code| DomainError::new(code, "Automation Call did not succeed")),
                        outcome.exit_code,
                    ),
                    Err(error) => (Some(error), None),
                };
                // Cancellation and the execution budget end the run whatever the step allows.
                effects.checkpoint()?;
                *result = result_facts(failure.as_ref(), exit_code);
                match failure {
                    Some(error) if on_failure.is_stop() => Err(error),
                    _ => Ok(()),
                }
            }
            AutomationAction::Sequence { children } => {
                for child in children {
                    visit(child, state, trigger, result, effects).await?;
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
                    ConditionSource::Result => evaluate_condition(condition, result),
                };
                if matched {
                    visit(then, state, trigger, result, effects).await
                } else if let Some(else_action) = else_action {
                    visit(else_action, state, trigger, result, effects).await
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
                    visit(action, state, trigger, result, effects).await?;
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

fn result_facts(
    failure: Option<&DomainError>,
    exit_code: Option<i32>,
) -> BTreeMap<String, ScalarValue> {
    let mut facts = BTreeMap::from([(
        "succeeded".to_owned(),
        ScalarValue::Boolean(failure.is_none()),
    )]);
    if let Some(token) = failure.and_then(|error| {
        serde_json::to_value(error.code)
            .ok()
            .and_then(|value| value.as_str().map(str::to_owned))
    }) {
        facts.insert("error_code".to_owned(), ScalarValue::String(token));
    }
    if let Some(exit_code) = exit_code {
        facts.insert(
            "exit_code".to_owned(),
            ScalarValue::Integer(i64::from(exit_code)),
        );
    }
    facts
}
