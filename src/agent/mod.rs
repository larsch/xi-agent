pub(crate) mod agent_loop;
pub mod compaction;
pub(crate) mod events;
pub mod file_tracker;
pub(crate) mod lifecycle;
pub(crate) mod loop_support;
pub(crate) mod runner;
pub mod system_prompt;
pub(crate) mod tool_batch;
pub(crate) mod tool_defs;
pub mod tool_output_log;
pub mod tools;
pub(crate) mod turn;
pub mod types;

#[cfg(test)]
mod tests;

pub use agent_loop::run_agent_loop;
pub use file_tracker::FileTracker;
pub use system_prompt::build_system_prompt;
pub use tool_output_log::ToolOutputLog;
pub use types::{
    AgentActivity, AgentEvent, AgentLoopConfig, CancelLevel, DefaultToolExecutor, ToolRegistry,
};
