use crate::{ACTION_SPECS, ScalarValue};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AutomationDescriptors {
    pub schema_version: u32,
    pub fields: Vec<FieldDescriptor>,
}
#[derive(Clone, Copy, Debug, Eq, JsonSchema, PartialEq, Serialize, Deserialize)]
pub enum LabelSource {
    #[serde(rename = "resource")]
    Resource,
    #[serde(rename = "wire_name")]
    WireName,
}
#[derive(Clone, Copy, Debug, Eq, JsonSchema, PartialEq, Serialize, Deserialize)]
pub enum Control {
    #[serde(rename = "switch")]
    Switch,
    #[serde(rename = "outlined_text")]
    OutlinedText,
    #[serde(rename = "single_choice")]
    SingleChoice,
    #[serde(rename = "optional")]
    Optional,
    #[serde(rename = "scalar_editor")]
    ScalarEditor,
    #[serde(rename = "key_scalar_table")]
    KeyScalarTable,
}
#[derive(Clone, Copy, Debug, Eq, JsonSchema, PartialEq, Serialize, Deserialize)]
pub enum ValueKind {
    #[serde(rename = "boolean")]
    Boolean,
    #[serde(rename = "string")]
    String,
    #[serde(rename = "integer")]
    Integer,
    #[serde(rename = "scalar")]
    Scalar,
}
#[derive(Clone, Copy, Debug, Eq, JsonSchema, PartialEq, Serialize, Deserialize)]
pub enum WrappedControl {
    #[serde(rename = "outlined_text")]
    OutlinedText,
    #[serde(rename = "single_choice")]
    SingleChoice,
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DescriptorOption {
    pub value: String,
    pub label_source: LabelSource,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label_resource: Option<String>,
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VisibilityCondition {
    pub canonical_path: String,
    pub values: Vec<ScalarValue>,
}
#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FieldDescriptor {
    pub canonical_path: String,
    pub order: u32,
    pub label_source: LabelSource,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label_resource: Option<String>,
    pub control: Control,
    pub sensitive: bool,
    pub validation: String,
    pub value_kind: ValueKind,
    pub required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<ScalarValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<DescriptorOption>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wrapped_control: Option<WrappedControl>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visible_when: Option<Vec<VisibilityCondition>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_rows: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_max_utf8_bytes: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scalar_kinds: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub string_max_utf8_bytes: Option<u32>,
}

fn ro(value: &str, label: &str) -> DescriptorOption {
    DescriptorOption {
        value: value.into(),
        label_source: LabelSource::Resource,
        label_resource: Some(label.into()),
    }
}
fn wo(value: &str) -> DescriptorOption {
    DescriptorOption {
        value: value.into(),
        label_source: LabelSource::WireName,
        label_resource: None,
    }
}
fn cv(path: &str, values: &[&str]) -> VisibilityCondition {
    VisibilityCondition {
        canonical_path: path.into(),
        values: values
            .iter()
            .map(|v| ScalarValue::String((*v).into()))
            .collect(),
    }
}
fn base(
    path: &str,
    order: u32,
    label_source: LabelSource,
    label: Option<&str>,
    control: Control,
    kind: ValueKind,
    required: bool,
) -> FieldDescriptor {
    FieldDescriptor {
        canonical_path: path.into(),
        order,
        label_source,
        label_resource: label.map(Into::into),
        control,
        sensitive: false,
        validation: "contract".into(),
        value_kind: kind,
        required,
        default: None,
        options: None,
        wrapped_control: None,
        visible_when: None,
        max_rows: None,
        key_max_utf8_bytes: None,
        scalar_kinds: None,
        string_max_utf8_bytes: None,
    }
}
fn resource(
    path: &str,
    order: u32,
    label: &str,
    control: Control,
    kind: ValueKind,
    required: bool,
) -> FieldDescriptor {
    base(
        path,
        order,
        LabelSource::Resource,
        Some(label),
        control,
        kind,
        required,
    )
}
fn wire(
    path: &str,
    order: u32,
    control: Control,
    kind: ValueKind,
    required: bool,
) -> FieldDescriptor {
    base(
        path,
        order,
        LabelSource::WireName,
        None,
        control,
        kind,
        required,
    )
}
fn choice(mut f: FieldDescriptor, options: Vec<DescriptorOption>) -> FieldDescriptor {
    f.options = Some(options);
    f
}
fn default(mut f: FieldDescriptor, value: ScalarValue) -> FieldDescriptor {
    f.default = Some(value);
    f
}
fn visible(mut f: FieldDescriptor, conditions: Vec<VisibilityCondition>) -> FieldDescriptor {
    f.visible_when = Some(conditions);
    f
}
fn optional(mut f: FieldDescriptor, wrapped: WrappedControl) -> FieldDescriptor {
    f.control = Control::Optional;
    f.required = false;
    f.wrapped_control = Some(wrapped);
    f
}
fn call_conditions(tool: &str, action: &str) -> Vec<VisibilityCondition> {
    vec![
        cv("/action/call/tool", &[tool]),
        cv("/action/call/action", &[action]),
    ]
}
fn call_visible(
    f: FieldDescriptor,
    tool: &str,
    action: &str,
    extra: Vec<VisibilityCondition>,
) -> FieldDescriptor {
    let mut c = call_conditions(tool, action);
    c.extend(extra);
    visible(f, c)
}

#[allow(clippy::vec_init_then_push)]
pub fn automation_descriptors() -> AutomationDescriptors {
    let mut v = Vec::new();
    v.push(resource(
        "/name",
        0,
        "automation_name",
        Control::OutlinedText,
        ValueKind::String,
        true,
    ));
    v.push(resource(
        "/enabled",
        1,
        "automation_enabled",
        Control::Switch,
        ValueKind::Boolean,
        true,
    ));
    v.push(choice(
        resource(
            "/trigger/type",
            0,
            "field_trigger_type",
            Control::SingleChoice,
            ValueKind::String,
            true,
        ),
        vec![
            ro("at", "trigger_at"),
            ro("interval", "trigger_interval"),
            ro("rrule", "trigger_rrule"),
            ro("event", "trigger_event"),
        ],
    ));
    v.push(visible(
        resource(
            "/trigger/at",
            1,
            "field_trigger_at",
            Control::OutlinedText,
            ValueKind::String,
            true,
        ),
        vec![cv("/trigger/type", &["at"])],
    ));
    v.push(visible(
        resource(
            "/trigger/every_ms",
            1,
            "field_trigger_every_ms",
            Control::OutlinedText,
            ValueKind::Integer,
            true,
        ),
        vec![cv("/trigger/type", &["interval"])],
    ));
    v.push(visible(
        resource(
            "/trigger/rrule",
            1,
            "field_trigger_rrule",
            Control::OutlinedText,
            ValueKind::String,
            true,
        ),
        vec![cv("/trigger/type", &["rrule"])],
    ));
    v.push(visible(
        resource(
            "/trigger/timezone",
            2,
            "field_trigger_timezone",
            Control::OutlinedText,
            ValueKind::String,
            true,
        ),
        vec![cv("/trigger/type", &["rrule"])],
    ));
    v.push(visible(
        resource(
            "/trigger/name",
            1,
            "field_trigger_event_name",
            Control::OutlinedText,
            ValueKind::String,
            true,
        ),
        vec![cv("/trigger/type", &["event"])],
    ));
    let mut table = resource(
        "/trigger/match",
        2,
        "field_trigger_event_match",
        Control::KeyScalarTable,
        ValueKind::Scalar,
        false,
    );
    table.max_rows = Some(32);
    table.key_max_utf8_bytes = Some(64);
    table.scalar_kinds = Some(vec![
        "null".into(),
        "boolean".into(),
        "integer".into(),
        "string".into(),
    ]);
    table.string_max_utf8_bytes = Some(4096);
    v.push(visible(table, vec![cv("/trigger/type", &["event"])]));
    v.push(choice(
        resource(
            "/action/type",
            0,
            "field_action_type",
            Control::SingleChoice,
            ValueKind::String,
            true,
        ),
        vec![
            ro("call", "action_node_call"),
            ro("sequence", "action_node_sequence"),
            ro("conditional", "action_node_conditional"),
            ro("repeat", "action_node_repeat"),
            ro("delay", "action_node_delay"),
            ro("set_state", "action_node_set_state"),
        ],
    ));
    let tools = ["filesystem", "command", "network", "visual", "android"];
    v.push(visible(
        choice(
            resource(
                "/action/call/tool",
                1,
                "task_detail_tool",
                Control::SingleChoice,
                ValueKind::String,
                true,
            ),
            tools.iter().map(|x| wo(x)).collect(),
        ),
        vec![cv("/action/type", &["call"])],
    ));
    for tool in tools {
        let actions: Vec<_> = ACTION_SPECS
            .iter()
            .filter(|spec| spec.tool == tool && spec.automation_compatible)
            .map(|spec| spec.action)
            .collect();
        v.push(visible(
            choice(
                resource(
                    "/action/call/action",
                    2,
                    "task_detail_action",
                    Control::SingleChoice,
                    ValueKind::String,
                    true,
                ),
                actions.iter().map(|x| wo(x)).collect(),
            ),
            vec![
                cv("/action/type", &["call"]),
                cv("/action/call/tool", &[tool]),
            ],
        ))
    }
    v.push(visible(
        choice(
            resource(
                "/action/conditional/condition/source",
                1,
                "field_condition_source",
                Control::SingleChoice,
                ValueKind::String,
                true,
            ),
            vec![wo("state"), wo("trigger")],
        ),
        vec![cv("/action/type", &["conditional"])],
    ));
    v.push(visible(
        resource(
            "/action/conditional/condition/key",
            2,
            "field_condition_key",
            Control::OutlinedText,
            ValueKind::String,
            true,
        ),
        vec![cv("/action/type", &["conditional"])],
    ));
    v.push(visible(
        choice(
            resource(
                "/action/conditional/condition/operator",
                3,
                "field_condition_operator",
                Control::SingleChoice,
                ValueKind::String,
                true,
            ),
            [
                "equals",
                "not_equals",
                "greater_than",
                "less_than",
                "exists",
            ]
            .iter()
            .map(|x| wo(x))
            .collect(),
        ),
        vec![cv("/action/type", &["conditional"])],
    ));
    let mut scalar = resource(
        "/action/conditional/condition/value",
        4,
        "field_condition_value",
        Control::ScalarEditor,
        ValueKind::Scalar,
        true,
    );
    scalar.scalar_kinds = Some(vec![
        "null".into(),
        "boolean".into(),
        "integer".into(),
        "string".into(),
    ]);
    scalar.string_max_utf8_bytes = Some(4096);
    v.push(visible(
        scalar,
        vec![
            cv("/action/type", &["conditional"]),
            cv(
                "/action/conditional/condition/operator",
                &["equals", "not_equals", "greater_than", "less_than"],
            ),
        ],
    ));
    v.push(visible(
        resource(
            "/action/repeat/count",
            1,
            "field_repeat_count",
            Control::OutlinedText,
            ValueKind::Integer,
            true,
        ),
        vec![cv("/action/type", &["repeat"])],
    ));
    v.push(visible(
        default(
            resource(
                "/action/repeat/delay_ms",
                2,
                "field_repeat_delay_ms",
                Control::OutlinedText,
                ValueKind::Integer,
                false,
            ),
            ScalarValue::Integer(0),
        ),
        vec![cv("/action/type", &["repeat"])],
    ));
    v.push(visible(
        resource(
            "/action/delay/duration_ms",
            1,
            "field_delay_duration_ms",
            Control::OutlinedText,
            ValueKind::Integer,
            true,
        ),
        vec![cv("/action/type", &["delay"])],
    ));
    v.push(visible(
        resource(
            "/action/set_state/key",
            1,
            "field_state_key",
            Control::OutlinedText,
            ValueKind::String,
            true,
        ),
        vec![cv("/action/type", &["set_state"])],
    ));
    let mut state_value = resource(
        "/action/set_state/value",
        2,
        "field_state_value",
        Control::ScalarEditor,
        ValueKind::Scalar,
        true,
    );
    state_value.scalar_kinds = Some(vec![
        "null".into(),
        "boolean".into(),
        "integer".into(),
        "string".into(),
    ]);
    state_value.string_max_utf8_bytes = Some(4096);
    v.push(visible(
        state_value,
        vec![cv("/action/type", &["set_state"])],
    ));

    add_filesystem(&mut v);
    add_command(&mut v);
    add_network(&mut v);
    add_visual(&mut v);
    add_android(&mut v);
    debug_assert_eq!(
        ACTION_SPECS
            .iter()
            .filter(|spec| spec.automation_compatible)
            .count(),
        7
    );
    AutomationDescriptors {
        schema_version: 1,
        fields: v,
    }
}

fn add_filesystem(v: &mut Vec<FieldDescriptor>) {
    let p = "/action/call/args/filesystem/manage/";
    let ops = ["mkdir", "copy", "move", "delete"];
    v.push(call_visible(
        choice(
            wire(
                &(p.to_owned() + "operation"),
                0,
                Control::SingleChoice,
                ValueKind::String,
                true,
            ),
            ops.iter().map(|x| wo(x)).collect(),
        ),
        "filesystem",
        "manage",
        vec![],
    ));
    for (prefix, which) in [
        ("target", vec!["mkdir", "delete"]),
        ("source", vec!["copy", "move"]),
        ("destination", vec!["copy", "move"]),
    ] {
        for (n, (leaf, (control, kind, opts))) in [
            ("type", (Control::SingleChoice, ValueKind::String, true)),
            ("value", (Control::OutlinedText, ValueKind::String, false)),
        ]
        .iter()
        .enumerate()
        {
            let mut f = wire(
                &format!("{p}{prefix}/{leaf}"),
                n as u32,
                *control,
                *kind,
                true,
            );
            if *opts {
                f = choice(f, vec![wo("path"), wo("content_uri")])
            };
            v.push(call_visible(
                f,
                "filesystem",
                "manage",
                vec![cv(&(p.to_owned() + "operation"), &which)],
            ))
        }
    }
    v.push(call_visible(
        default(
            wire(
                &(p.to_owned() + "parents"),
                2,
                Control::Switch,
                ValueKind::Boolean,
                false,
            ),
            ScalarValue::Boolean(false),
        ),
        "filesystem",
        "manage",
        vec![cv(&(p.to_owned() + "operation"), &["mkdir"])],
    ));
    v.push(call_visible(
        default(
            wire(
                &(p.to_owned() + "recursive"),
                2,
                Control::Switch,
                ValueKind::Boolean,
                false,
            ),
            ScalarValue::Boolean(false),
        ),
        "filesystem",
        "manage",
        vec![cv(
            &(p.to_owned() + "operation"),
            &["copy", "move", "delete"],
        )],
    ));
    v.push(call_visible(
        default(
            wire(
                &(p.to_owned() + "overwrite"),
                3,
                Control::Switch,
                ValueKind::Boolean,
                false,
            ),
            ScalarValue::Boolean(false),
        ),
        "filesystem",
        "manage",
        vec![cv(&(p.to_owned() + "operation"), &["copy", "move"])],
    ));
    let d = "/action/call/args/filesystem/download/";
    v.push(call_visible(
        wire(
            &(d.to_owned() + "url"),
            0,
            Control::OutlinedText,
            ValueKind::String,
            true,
        ),
        "filesystem",
        "download",
        vec![],
    ));
    v.push(call_visible(
        choice(
            wire(
                &(d.to_owned() + "destination/type"),
                1,
                Control::SingleChoice,
                ValueKind::String,
                true,
            ),
            vec![wo("path"), wo("content_uri")],
        ),
        "filesystem",
        "download",
        vec![],
    ));
    v.push(call_visible(
        wire(
            &(d.to_owned() + "destination/value"),
            2,
            Control::OutlinedText,
            ValueKind::String,
            true,
        ),
        "filesystem",
        "download",
        vec![],
    ));
    v.push(call_visible(
        default(
            wire(
                &(d.to_owned() + "overwrite"),
                3,
                Control::Switch,
                ValueKind::Boolean,
                false,
            ),
            ScalarValue::Boolean(false),
        ),
        "filesystem",
        "download",
        vec![],
    ));
    v.push(call_visible(
        default(
            wire(
                &(d.to_owned() + "timeout_ms"),
                4,
                Control::OutlinedText,
                ValueKind::Integer,
                false,
            ),
            ScalarValue::Integer(120000),
        ),
        "filesystem",
        "download",
        vec![],
    ));
}
fn add_command(v: &mut Vec<FieldDescriptor>) {
    let p = "/action/call/args/command/run/";
    let specs = [
        (
            "command",
            Control::OutlinedText,
            ValueKind::String,
            true,
            None,
        ),
        (
            "run_as",
            Control::SingleChoice,
            ValueKind::String,
            true,
            None,
        ),
        ("cwd", Control::OutlinedText, ValueKind::String, false, None),
        (
            "stdin",
            Control::OutlinedText,
            ValueKind::String,
            false,
            None,
        ),
        (
            "timeout_ms",
            Control::OutlinedText,
            ValueKind::Integer,
            false,
            Some(ScalarValue::Integer(30000)),
        ),
        (
            "max_output_bytes",
            Control::OutlinedText,
            ValueKind::Integer,
            false,
            Some(ScalarValue::Integer(65536)),
        ),
        (
            "as_task",
            Control::Switch,
            ValueKind::Boolean,
            false,
            Some(ScalarValue::Boolean(false)),
        ),
    ];
    for (i, (name, control, kind, required, def)) in specs.into_iter().enumerate() {
        let mut f = wire(&(p.to_owned() + name), i as u32, control, kind, required);
        if name == "run_as" {
            f = choice(f, vec![wo("app"), wo("shell"), wo("root")])
        }
        if !required && def.is_none() {
            f = optional(f, WrappedControl::OutlinedText)
        }
        if let Some(x) = def {
            f = default(f, x)
        }
        v.push(call_visible(f, "command", "run", vec![]))
    }
}
fn add_network(v: &mut Vec<FieldDescriptor>) {
    let p = "/action/call/args/network/diagnose/";
    v.push(call_visible(
        choice(
            wire(
                &(p.to_owned() + "test"),
                0,
                Control::SingleChoice,
                ValueKind::String,
                true,
            ),
            ["connectivity", "dns", "tcp", "tls", "route"]
                .iter()
                .map(|x| wo(x))
                .collect(),
        ),
        "network",
        "diagnose",
        vec![],
    ));
    let defs = [
        ("name", 1, ValueKind::String, vec!["dns"], None),
        (
            "record_type",
            2,
            ValueKind::String,
            vec!["dns"],
            Some(ScalarValue::String("A".into())),
        ),
        ("host", 1, ValueKind::String, vec!["tcp", "tls"], None),
        ("port", 2, ValueKind::Integer, vec!["tcp"], None),
        (
            "port",
            2,
            ValueKind::Integer,
            vec!["tls"],
            Some(ScalarValue::Integer(443)),
        ),
        ("server_name", 3, ValueKind::String, vec!["tls"], None),
        (
            "timeout_ms",
            3,
            ValueKind::Integer,
            vec!["tcp"],
            Some(ScalarValue::Integer(5000)),
        ),
        (
            "timeout_ms",
            4,
            ValueKind::Integer,
            vec!["tls"],
            Some(ScalarValue::Integer(5000)),
        ),
        ("destination_ip", 1, ValueKind::String, vec!["route"], None),
    ];
    for (name, order, kind, tests, def) in defs {
        let mut f = wire(
            &(p.to_owned() + name),
            order,
            if name == "record_type" {
                Control::SingleChoice
            } else {
                Control::OutlinedText
            },
            kind,
            def.is_none() && name != "server_name",
        );
        if name == "record_type" {
            f = choice(f, vec![wo("A"), wo("AAAA")])
        }
        if name == "server_name" {
            f = optional(f, WrappedControl::OutlinedText)
        }
        if let Some(x) = def {
            f = default(f, x)
        }
        v.push(call_visible(
            f,
            "network",
            "diagnose",
            vec![cv(&(p.to_owned() + "test"), &tests)],
        ))
    }
}
fn add_visual(v: &mut Vec<FieldDescriptor>) {
    let p = "/action/call/args/visual/interact/";
    v.push(call_visible(
        choice(
            wire(
                &(p.to_owned() + "operation"),
                0,
                Control::SingleChoice,
                ValueKind::String,
                true,
            ),
            ["tap", "long_press", "swipe", "text", "key"]
                .iter()
                .map(|x| wo(x))
                .collect(),
        ),
        "visual",
        "interact",
        vec![],
    ));
    v.push(call_visible(
        choice(
            wire(
                &(p.to_owned() + "target"),
                1,
                Control::SingleChoice,
                ValueKind::String,
                true,
            ),
            vec![wo("node"), wo("coordinate")],
        ),
        "visual",
        "interact",
        vec![cv(&(p.to_owned() + "operation"), &["tap", "long_press"])],
    ));
    v.push(call_visible(
        wire(
            &(p.to_owned() + "node_ref"),
            2,
            Control::OutlinedText,
            ValueKind::String,
            true,
        ),
        "visual",
        "interact",
        vec![
            cv(&(p.to_owned() + "operation"), &["tap", "long_press"]),
            cv(&(p.to_owned() + "target"), &["node"]),
        ],
    ));
    for (i, name) in ["observation_id", "x", "y"].iter().enumerate() {
        v.push(call_visible(
            wire(
                &(p.to_owned() + name),
                (i + 2) as u32,
                Control::OutlinedText,
                if *name == "observation_id" {
                    ValueKind::String
                } else {
                    ValueKind::Integer
                },
                true,
            ),
            "visual",
            "interact",
            vec![
                cv(&(p.to_owned() + "operation"), &["tap", "long_press"]),
                cv(&(p.to_owned() + "target"), &["coordinate"]),
            ],
        ))
    }
    for (i, name) in ["observation_id", "from_x", "from_y", "to_x", "to_y"]
        .iter()
        .enumerate()
    {
        v.push(call_visible(
            wire(
                &(p.to_owned() + name),
                (i + 1) as u32,
                Control::OutlinedText,
                if *name == "observation_id" {
                    ValueKind::String
                } else {
                    ValueKind::Integer
                },
                true,
            ),
            "visual",
            "interact",
            vec![cv(&(p.to_owned() + "operation"), &["swipe"])],
        ))
    }
    v.push(call_visible(
        default(
            wire(
                &(p.to_owned() + "duration_ms"),
                6,
                Control::OutlinedText,
                ValueKind::Integer,
                false,
            ),
            ScalarValue::Integer(300),
        ),
        "visual",
        "interact",
        vec![cv(&(p.to_owned() + "operation"), &["swipe"])],
    ));
    v.push(call_visible(
        wire(
            &(p.to_owned() + "text"),
            1,
            Control::OutlinedText,
            ValueKind::String,
            true,
        ),
        "visual",
        "interact",
        vec![cv(&(p.to_owned() + "operation"), &["text"])],
    ));
    v.push(call_visible(
        optional(
            wire(
                &(p.to_owned() + "node_ref"),
                2,
                Control::OutlinedText,
                ValueKind::String,
                false,
            ),
            WrappedControl::OutlinedText,
        ),
        "visual",
        "interact",
        vec![cv(&(p.to_owned() + "operation"), &["text"])],
    ));
    v.push(call_visible(
        wire(
            &(p.to_owned() + "key_code"),
            1,
            Control::OutlinedText,
            ValueKind::Integer,
            true,
        ),
        "visual",
        "interact",
        vec![cv(&(p.to_owned() + "operation"), &["key"])],
    ));
    v.push(call_visible(
        default(
            wire(
                &(p.to_owned() + "meta_state"),
                2,
                Control::OutlinedText,
                ValueKind::Integer,
                false,
            ),
            ScalarValue::Integer(0),
        ),
        "visual",
        "interact",
        vec![cv(&(p.to_owned() + "operation"), &["key"])],
    ));
}
fn add_android(v: &mut Vec<FieldDescriptor>) {
    let p = "/action/call/args/android/launch/";
    v.push(call_visible(
        choice(
            wire(
                &(p.to_owned() + "operation"),
                0,
                Control::SingleChoice,
                ValueKind::String,
                true,
            ),
            vec![wo("package"), wo("component")],
        ),
        "android",
        "launch",
        vec![],
    ));
    v.push(call_visible(
        wire(
            &(p.to_owned() + "package_name"),
            1,
            Control::OutlinedText,
            ValueKind::String,
            true,
        ),
        "android",
        "launch",
        vec![],
    ));
    v.push(call_visible(
        wire(
            &(p.to_owned() + "class_name"),
            2,
            Control::OutlinedText,
            ValueKind::String,
            true,
        ),
        "android",
        "launch",
        vec![cv(&(p.to_owned() + "operation"), &["component"])],
    ));
    let c = "/action/call/args/android/clipboard/";
    v.push(call_visible(
        choice(
            wire(
                &(c.to_owned() + "operation"),
                0,
                Control::SingleChoice,
                ValueKind::String,
                true,
            ),
            vec![wo("read"), wo("write"), wo("clear")],
        ),
        "android",
        "clipboard",
        vec![],
    ));
    v.push(call_visible(
        wire(
            &(c.to_owned() + "text"),
            1,
            Control::OutlinedText,
            ValueKind::String,
            true,
        ),
        "android",
        "clipboard",
        vec![cv(&(c.to_owned() + "operation"), &["write"])],
    ));
}

fn scalar_key(value: &ScalarValue) -> String {
    serde_json::to_string(value).expect("descriptor scalar is serializable")
}

fn conditions_are_mutually_exclusive(
    left: &[VisibilityCondition],
    right: &[VisibilityCondition],
) -> bool {
    left.iter().any(|a| {
        right.iter().any(|b| {
            if a.canonical_path != b.canonical_path {
                return false;
            }
            let av: BTreeSet<_> = a.values.iter().map(scalar_key).collect();
            let bv: BTreeSet<_> = b.values.iter().map(scalar_key).collect();
            av.is_disjoint(&bv)
        })
    })
}

pub fn validate_automation_descriptors(value: &AutomationDescriptors) -> Result<(), String> {
    if value.schema_version != 1 {
        return Err("descriptor schema_version must be 1".into());
    }
    for field in &value.fields {
        if !field.canonical_path.starts_with('/') || field.canonical_path.contains("//") {
            return Err(format!("invalid canonical path {}", field.canonical_path));
        }
        if field.validation != "contract" || field.sensitive {
            return Err(format!(
                "invalid base metadata for {}",
                field.canonical_path
            ));
        }
        match field.label_source {
            LabelSource::Resource if field.label_resource.as_deref().unwrap_or("").is_empty() => {
                return Err(format!(
                    "resource label missing for {}",
                    field.canonical_path
                ));
            }
            LabelSource::WireName if field.label_resource.is_some() => {
                return Err(format!(
                    "wire label has resource for {}",
                    field.canonical_path
                ));
            }
            _ => {}
        }
        if let Some(conditions) = &field.visible_when {
            if conditions.is_empty() || conditions.len() > 4 {
                return Err(format!(
                    "invalid visibility count for {}",
                    field.canonical_path
                ));
            }
            for condition in conditions {
                let unique: BTreeSet<_> = condition.values.iter().map(scalar_key).collect();
                if unique.is_empty() || unique.len() != condition.values.len() {
                    return Err(format!(
                        "invalid visibility values for {}",
                        field.canonical_path
                    ));
                }
            }
        }
        if let Some(options) = &field.options {
            if options.is_empty() {
                return Err(format!("empty options for {}", field.canonical_path));
            }
            let mut values = BTreeSet::new();
            for option in options {
                if !values.insert(&option.value) {
                    return Err(format!("duplicate option for {}", field.canonical_path));
                }
                match option.label_source {
                    LabelSource::Resource
                        if option.label_resource.as_deref().unwrap_or("").is_empty() =>
                    {
                        return Err(format!(
                            "option resource missing for {}",
                            field.canonical_path
                        ));
                    }
                    LabelSource::WireName if option.label_resource.is_some() => {
                        return Err(format!(
                            "wire option has resource for {}",
                            field.canonical_path
                        ));
                    }
                    _ => {}
                }
            }
        }
        let no_table = field.max_rows.is_none()
            && field.key_max_utf8_bytes.is_none()
            && field.scalar_kinds.is_none()
            && field.string_max_utf8_bytes.is_none();
        match field.control {
            Control::Switch => {
                if field.value_kind != ValueKind::Boolean
                    || field.options.is_some()
                    || field.wrapped_control.is_some()
                    || !no_table
                    || !matches!(field.default, None | Some(ScalarValue::Boolean(_)))
                {
                    return Err(format!("invalid switch {}", field.canonical_path));
                }
            }
            Control::OutlinedText => {
                let default_ok = matches!(
                    (&field.value_kind, &field.default),
                    (ValueKind::String, None | Some(ScalarValue::String(_)))
                        | (ValueKind::Integer, None | Some(ScalarValue::Integer(_)))
                );
                if !default_ok
                    || field.options.is_some()
                    || field.wrapped_control.is_some()
                    || !no_table
                {
                    return Err(format!("invalid outlined text {}", field.canonical_path));
                }
            }
            Control::SingleChoice => {
                let options = field.options.as_ref().ok_or_else(|| {
                    format!("single choice options missing for {}", field.canonical_path)
                })?;
                let default_ok = match &field.default {
                    None => true,
                    Some(ScalarValue::String(value)) => options.iter().any(|o| o.value == *value),
                    _ => false,
                };
                if field.value_kind != ValueKind::String
                    || !default_ok
                    || field.wrapped_control.is_some()
                    || !no_table
                {
                    return Err(format!("invalid single choice {}", field.canonical_path));
                }
            }
            Control::Optional => {
                let choice_ok = field.wrapped_control != Some(WrappedControl::SingleChoice)
                    || field
                        .options
                        .as_ref()
                        .is_some_and(|options| !options.is_empty());
                if field.required
                    || field.default.is_some()
                    || field.wrapped_control.is_none()
                    || !choice_ok
                    || !no_table
                {
                    return Err(format!("invalid optional {}", field.canonical_path));
                }
            }
            Control::ScalarEditor => {
                if field.value_kind != ValueKind::Scalar
                    || !field.required
                    || field.default.is_some()
                    || field.options.is_some()
                    || field.wrapped_control.is_some()
                    || field.scalar_kinds.as_deref()
                        != Some(&[
                            "null".into(),
                            "boolean".into(),
                            "integer".into(),
                            "string".into(),
                        ])
                    || field.string_max_utf8_bytes != Some(4096)
                    || field.max_rows.is_some()
                    || field.key_max_utf8_bytes.is_some()
                {
                    return Err(format!("invalid scalar editor {}", field.canonical_path));
                }
            }
            Control::KeyScalarTable => {
                if field.value_kind != ValueKind::Scalar
                    || field.required
                    || field.default.is_some()
                    || field.options.is_some()
                    || field.wrapped_control.is_some()
                    || field.max_rows != Some(32)
                    || field.key_max_utf8_bytes != Some(64)
                    || field.scalar_kinds.as_deref()
                        != Some(&[
                            "null".into(),
                            "boolean".into(),
                            "integer".into(),
                            "string".into(),
                        ])
                    || field.string_max_utf8_bytes != Some(4096)
                {
                    return Err(format!("invalid key-scalar table {}", field.canonical_path));
                }
            }
        }
    }
    for (index, left) in value.fields.iter().enumerate() {
        for right in value.fields.iter().skip(index + 1) {
            if left.canonical_path == right.canonical_path
                && !conditions_are_mutually_exclusive(
                    left.visible_when.as_deref().unwrap_or(&[]),
                    right.visible_when.as_deref().unwrap_or(&[]),
                )
            {
                return Err(format!("ambiguous descriptor path {}", left.canonical_path));
            }
        }
    }
    for spec in ACTION_SPECS
        .iter()
        .filter(|spec| spec.automation_compatible)
    {
        let (tool, action) = (spec.tool, spec.action);
        let prefix = format!("/action/call/args/{tool}/{action}/");
        if !value
            .fields
            .iter()
            .any(|field| field.canonical_path.starts_with(&prefix))
        {
            return Err(format!("missing call descriptors for {tool}.{action}"));
        }
    }
    let target = value
        .fields
        .iter()
        .find(|field| field.canonical_path == "/action/call/args/visual/interact/target")
        .ok_or_else(|| "visual interaction target descriptor missing".to_string())?;
    if target.control != Control::SingleChoice
        || target
            .options
            .as_ref()
            .map(|options| options.iter().map(|o| o.value.as_str()).collect::<Vec<_>>())
            != Some(vec!["node", "coordinate"])
    {
        return Err("visual interaction target descriptor is not the public tagged union".into());
    }
    Ok(())
}
