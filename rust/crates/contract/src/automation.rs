use crate::{
    AndroidClipboardInput, AndroidLaunchInput, AutomationExecutionState, AutomationId,
    CommandRunInput, ExecutionId, FilesystemDownloadInput, FilesystemManageInput,
    NetworkDiagnoseInput, ScalarValue, TaskId, VisualInteractInput,
};
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, de};
use std::collections::BTreeMap;
fn d20() -> u32 {
    20
}
fn d100() -> u32 {
    100
}
fn d10s() -> u64 {
    10_000
}

#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "type")]
pub enum AutomationTrigger {
    #[serde(rename = "at")]
    At { at: String },
    #[serde(rename = "interval")]
    Interval {
        #[schemars(range(min = 60000u64, max = 2592000000u64))]
        every_ms: u64,
    },
    #[serde(rename = "rrule")]
    Rrule { rrule: String, timezone: String },
    #[serde(rename = "event")]
    Event {
        name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        #[schemars(length(max = 32))]
        r#match: Option<BTreeMap<String, ScalarValue>>,
    },
}
#[derive(Clone, Copy, Debug, Eq, JsonSchema, PartialEq, Serialize, Deserialize)]
pub enum ConditionSource {
    #[serde(rename = "state")]
    State,
    #[serde(rename = "trigger")]
    Trigger,
    /// The outcome of the Call visited last in this execution: `succeeded`, `error_code` when it
    /// failed, and `exit_code` when it ran a command.
    #[serde(rename = "result")]
    Result,
}
#[derive(Clone, Copy, Debug, Eq, JsonSchema, PartialEq, Serialize, Deserialize)]
pub enum ConditionOperator {
    #[serde(rename = "equals")]
    Equals,
    #[serde(rename = "not_equals")]
    NotEquals,
    #[serde(rename = "greater_than")]
    GreaterThan,
    #[serde(rename = "less_than")]
    LessThan,
    #[serde(rename = "exists")]
    Exists,
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AutomationCondition {
    pub source: ConditionSource,
    pub key: String,
    pub operator: ConditionOperator,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<ScalarValue>,
}

#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(tag = "tool")]
#[schemars(deny_unknown_fields)]
pub enum AutomationCompatibleCall {
    #[serde(rename = "filesystem")]
    Filesystem {
        #[serde(flatten)]
        call: AutomationFilesystemCall,
    },
    #[serde(rename = "command")]
    Command {
        #[serde(flatten)]
        call: AutomationCommandCall,
    },
    #[serde(rename = "network")]
    Network {
        #[serde(flatten)]
        call: AutomationNetworkCall,
    },
    #[serde(rename = "visual")]
    Visual {
        #[serde(flatten)]
        call: AutomationVisualCall,
    },
    #[serde(rename = "android")]
    Android {
        #[serde(flatten)]
        call: AutomationAndroidCall,
    },
}

#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "action", content = "args")]
pub enum AutomationFilesystemCall {
    #[serde(rename = "manage")]
    Manage(FilesystemManageInput),
    #[serde(rename = "download")]
    Download(FilesystemDownloadInput),
}

#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "action", content = "args")]
pub enum AutomationCommandCall {
    #[serde(rename = "run")]
    Run(CommandRunInput),
}

#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "action", content = "args")]
pub enum AutomationNetworkCall {
    #[serde(rename = "diagnose")]
    Diagnose(NetworkDiagnoseInput),
}

#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "action", content = "args")]
pub enum AutomationVisualCall {
    #[serde(rename = "interact")]
    Interact(VisualInteractInput),
    /// Observes the screen when the step runs, finds the first node that matches, and acts on it
    /// through that fresh observation, so a saved Automation never holds a stale reference.
    #[serde(rename = "element")]
    Element(AutomationElementInput),
}

crate::common::string_enum!(ElementOperation { Tap=>"tap", LongPress=>"long_press", Text=>"text" });
crate::common::string_enum!(ElementMatch { Text=>"text", TextContains=>"text_contains", Description=>"description", ResourceId=>"resource_id" });

#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AutomationElementInput {
    pub operation: ElementOperation,
    /// `text` and `description` match exactly, `text_contains` matches part of the text or the
    /// description, and `resource_id` matches the full id or the part after `:id/`.
    pub by: ElementMatch,
    #[schemars(length(min = 1, max = 1024))]
    pub value: String,
    /// The text to enter; required for `operation: text` and rejected otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// How long to keep observing for the element before the step fails with NOT_FOUND.
    #[serde(default = "d10s")]
    #[schemars(range(max = 60000))]
    pub wait_ms: u64,
}

crate::common::string_enum!(AutomationStepFailure { Stop=>"stop", Continue=>"continue" });

#[allow(clippy::derivable_impls)]
impl Default for AutomationStepFailure {
    fn default() -> Self {
        Self::Stop
    }
}

impl AutomationStepFailure {
    pub fn is_stop(&self) -> bool {
        *self == Self::Stop
    }
}

#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "action", content = "args")]
pub enum AutomationAndroidCall {
    #[serde(rename = "launch")]
    Launch(AndroidLaunchInput),
    #[serde(rename = "clipboard")]
    Clipboard(AndroidClipboardInput),
}

#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "type")]
#[schemars(deny_unknown_fields)]
pub enum AutomationAction {
    #[serde(rename = "call")]
    Call {
        #[serde(flatten)]
        call: AutomationCompatibleCall,
        /// `stop` (default) ends the execution as failed; `continue` records the failure for a
        /// later `result` condition and goes on.
        #[serde(default, skip_serializing_if = "AutomationStepFailure::is_stop")]
        on_failure: AutomationStepFailure,
    },
    #[serde(rename = "sequence")]
    Sequence {
        #[schemars(length(min = 1, max = 64))]
        children: Vec<AutomationAction>,
    },
    #[serde(rename = "conditional")]
    Conditional {
        condition: AutomationCondition,
        then: Box<AutomationAction>,
        #[serde(rename = "else", skip_serializing_if = "Option::is_none")]
        else_action: Option<Box<AutomationAction>>,
    },
    #[serde(rename = "repeat")]
    Repeat {
        #[schemars(range(min = 1, max = 1000))]
        count: u32,
        action: Box<AutomationAction>,
        #[serde(default)]
        #[schemars(range(max = 86400000))]
        delay_ms: u64,
    },
    #[serde(rename = "delay")]
    Delay {
        #[schemars(range(min = 1, max = 86400000))]
        duration_ms: u64,
    },
    #[serde(rename = "set_state")]
    SetState { key: String, value: ScalarValue },
}

#[derive(Deserialize)]
#[serde(tag = "type")]
enum AutomationActionWire {
    #[serde(rename = "call")]
    Call {
        #[serde(flatten)]
        call: AutomationCompatibleCall,
        #[serde(default)]
        on_failure: AutomationStepFailure,
    },
    #[serde(rename = "sequence")]
    Sequence { children: Vec<AutomationAction> },
    #[serde(rename = "conditional")]
    Conditional {
        condition: AutomationCondition,
        then: Box<AutomationAction>,
        #[serde(rename = "else")]
        else_action: Option<Box<AutomationAction>>,
    },
    #[serde(rename = "repeat")]
    Repeat {
        count: u32,
        action: Box<AutomationAction>,
        #[serde(default)]
        delay_ms: u64,
    },
    #[serde(rename = "delay")]
    Delay { duration_ms: u64 },
    #[serde(rename = "set_state")]
    SetState { key: String, value: ScalarValue },
}

impl From<AutomationActionWire> for AutomationAction {
    fn from(value: AutomationActionWire) -> Self {
        match value {
            AutomationActionWire::Call { call, on_failure } => Self::Call { call, on_failure },
            AutomationActionWire::Sequence { children } => Self::Sequence { children },
            AutomationActionWire::Conditional {
                condition,
                then,
                else_action,
            } => Self::Conditional {
                condition,
                then,
                else_action,
            },
            AutomationActionWire::Repeat {
                count,
                action,
                delay_ms,
            } => Self::Repeat {
                count,
                action,
                delay_ms,
            },
            AutomationActionWire::Delay { duration_ms } => Self::Delay { duration_ms },
            AutomationActionWire::SetState { key, value } => Self::SetState { key, value },
        }
    }
}

impl<'de> Deserialize<'de> for AutomationAction {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        let object = value
            .as_object()
            .ok_or_else(|| de::Error::custom("Automation action must be an object"))?;
        let action_type = object
            .get("type")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| de::Error::custom("Automation action type must be a string"))?;
        let allowed: &[&str] = match action_type {
            "call" => &["type", "tool", "action", "args", "on_failure"],
            "sequence" => &["type", "children"],
            "conditional" => &["type", "condition", "then", "else"],
            "repeat" => &["type", "count", "action", "delay_ms"],
            "delay" => &["type", "duration_ms"],
            "set_state" => &["type", "key", "value"],
            _ => return Err(de::Error::custom("unknown Automation action type")),
        };
        if object.keys().any(|key| !allowed.contains(&key.as_str())) {
            return Err(de::Error::custom("unknown Automation action field"));
        }
        serde_json::from_value::<AutomationActionWire>(value)
            .map(Into::into)
            .map_err(de::Error::custom)
    }
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Automation {
    pub automation_id: AutomationId,
    pub name: String,
    pub enabled: bool,
    pub trigger: AutomationTrigger,
    pub action: AutomationAction,
    #[schemars(length(max = 64))]
    pub state: BTreeMap<String, ScalarValue>,
    pub revision: u64,
    pub created_at: String,
    pub updated_at: String,
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AutomationExecutionSummary {
    pub execution_id: ExecutionId,
    pub task_id: TaskId,
    pub state: AutomationExecutionState,
    pub triggered_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AutomationSummary {
    pub automation_id: AutomationId,
    pub name: String,
    pub enabled: bool,
    pub revision: u64,
    pub updated_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_execution: Option<AutomationExecutionSummary>,
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AutomationListInput {
    #[serde(default = "d100")]
    #[schemars(range(min = 1, max = 500))]
    pub limit: u32,
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AutomationListResult {
    pub automations: Vec<AutomationSummary>,
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AutomationGetInput {
    pub automation_id: AutomationId,
    #[serde(default = "d20")]
    #[schemars(range(min = 1, max = 100))]
    pub history_limit: u32,
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AutomationGetResult {
    pub automation: Automation,
    pub history: Vec<AutomationExecutionSummary>,
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, untagged)]
pub enum AutomationSaveInput {
    Create(AutomationCreateInput),
    Update(AutomationUpdateInput),
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AutomationCreateInput {
    pub name: String,
    pub enabled: bool,
    pub trigger: AutomationTrigger,
    pub action: AutomationAction,
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AutomationUpdateInput {
    pub automation_id: AutomationId,
    pub expected_revision: u64,
    pub name: String,
    pub enabled: bool,
    pub trigger: AutomationTrigger,
    pub action: AutomationAction,
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AutomationSetEnabledInput {
    pub automation_id: AutomationId,
    pub enabled: bool,
    pub expected_revision: u64,
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AutomationRunInput {
    pub automation_id: AutomationId,
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AutomationRunResult {
    pub automation_id: AutomationId,
    /// The run starts once the scheduler admits it; its execution then appears in `get` history.
    pub run_requested: crate::True,
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AutomationDeleteInput {
    pub automation_id: AutomationId,
    pub expected_revision: u64,
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AutomationDeleteResult {
    pub automation_id: AutomationId,
    pub deleted: crate::True,
    pub previous_revision: u64,
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AutomationTaskResult {
    pub automation_id: AutomationId,
    pub execution_id: ExecutionId,
    pub completed: crate::True,
}
