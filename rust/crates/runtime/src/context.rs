//! The static `context.catalog` projection (R-CONTEXT-006..007, S-CONTEXT-001). It is a build-time
//! table over the generated action registry: it reads no capability, persists nothing and returns
//! the same answer whatever grants the Runtime currently observes.

use contract::{
    ACTION_SPECS, CatalogAction, ContextCatalogInput, ContextCatalogResult, ContextDetail,
    ErrorCode, MotherTool, ROOT_TOOL_ORDER,
};
use domain::DomainError;

pub fn context_catalog(input: &ContextCatalogInput) -> Result<ContextCatalogResult, DomainError> {
    let root_tools = ROOT_TOOL_ORDER.to_vec();
    if input.namespace.is_empty() {
        return Ok(ContextCatalogResult {
            current_namespace: String::new(),
            root_tools,
            parent: None,
            siblings: Vec::new(),
            actions: Vec::new(),
        });
    }
    let tool = ROOT_TOOL_ORDER
        .into_iter()
        .find(|tool| namespace_token(*tool) == input.namespace)
        .ok_or_else(|| {
            DomainError::new(
                ErrorCode::NotFound,
                "catalog namespace is not a mother tool",
            )
        })?;
    let full = matches!(input.detail, ContextDetail::Full);
    let actions = ACTION_SPECS
        .iter()
        .filter(|spec| spec.tool == input.namespace)
        .map(|spec| CatalogAction {
            name: spec.action.to_owned(),
            automation_compatible: spec.automation_compatible,
            capability_requirement: full.then(|| spec.capability_requirement.to_owned()),
        })
        .collect();
    Ok(ContextCatalogResult {
        current_namespace: input.namespace.clone(),
        root_tools,
        parent: Some(String::new()),
        siblings: ROOT_TOOL_ORDER
            .into_iter()
            .filter(|other| *other != tool)
            .collect(),
        actions,
    })
}

const fn namespace_token(tool: MotherTool) -> &'static str {
    match tool {
        MotherTool::Context => "context",
        MotherTool::Filesystem => "filesystem",
        MotherTool::Command => "command",
        MotherTool::Network => "network",
        MotherTool::Visual => "visual",
        MotherTool::Android => "android",
        MotherTool::Automation => "automation",
        MotherTool::TaskControl => "task_control",
    }
}
