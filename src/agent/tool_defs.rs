use crate::agent::types::ToolRegistry;
use crate::llm::ToolDefinition;

/// Build a sorted list of [`ToolDefinition`]s from the tool registry.
///
/// Sorted alphabetically by name so the serialized request body is
/// deterministic across process restarts, keeping the LLM provider's
/// prompt cache stable.
pub(crate) fn build_sorted_tool_defs(tools: &ToolRegistry) -> Vec<ToolDefinition> {
    let mut defs: Vec<ToolDefinition> = tools
        .values()
        .map(|t| ToolDefinition {
            name: t.name().to_string(),
            description: t.description().to_string(),
            parameters: t.parameters_schema(),
            streaming_field: t.streaming_field().map(str::to_owned),
        })
        .collect();
    defs.sort_by(|a, b| a.name.cmp(&b.name));
    defs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_registry_produces_no_definitions() {
        let registry = ToolRegistry::new();
        assert!(build_sorted_tool_defs(&registry).is_empty());
    }
}
