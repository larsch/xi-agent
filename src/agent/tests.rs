use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use futures_util::stream;
use tokio::sync::mpsc;

use crate::agent::tools::ask_user::AskUserTool;
use crate::agent::types::{AgentEvent, AskUserResponse, Tool};
use crate::agent::{AgentLoopConfig, DefaultToolExecutor, run_agent_loop};
use crate::app_event::AppEvent;
use crate::llm::{
    AssistantPhase, LlmEvent, LlmProvider, LlmRequestContext, LlmStream, Message, ModelListFuture,
    ToolDefinition, UsageStats,
};

// ── MockProvider ──────────────────────────────────────────────────────────────

/// A fake LLM provider that returns pre-canned sequences of `LlmEvent`s,
/// one `Vec<LlmEvent>` per turn.
struct MockProvider {
    turns: Arc<Mutex<std::collections::VecDeque<Vec<LlmEvent>>>>,
}

impl MockProvider {
    fn new(turns: Vec<Vec<LlmEvent>>) -> Self {
        Self {
            turns: Arc::new(Mutex::new(turns.into())),
        }
    }
}

impl LlmProvider for MockProvider {
    fn stream_chat(&self, messages: Vec<Message>, context: LlmRequestContext) -> LlmStream {
        self.stream_chat_with_tools(messages, vec![], context)
    }

    fn stream_chat_with_tools(
        &self,
        _messages: Vec<Message>,
        _tools: Vec<ToolDefinition>,
        _context: LlmRequestContext,
    ) -> LlmStream {
        let events = self.turns.lock().unwrap().pop_front().unwrap_or_default();
        Box::pin(stream::iter(events))
    }

    fn list_models(&self) -> ModelListFuture {
        Box::pin(async { Ok(vec![]) })
    }
}

struct DelayedMockProvider {
    turns: Arc<Mutex<std::collections::VecDeque<Vec<LlmEvent>>>>,
    delay: std::time::Duration,
}

impl DelayedMockProvider {
    fn new(turns: Vec<Vec<LlmEvent>>, delay: std::time::Duration) -> Self {
        Self {
            turns: Arc::new(Mutex::new(turns.into())),
            delay,
        }
    }
}

impl LlmProvider for DelayedMockProvider {
    fn stream_chat(&self, messages: Vec<Message>, context: LlmRequestContext) -> LlmStream {
        self.stream_chat_with_tools(messages, vec![], context)
    }

    fn stream_chat_with_tools(
        &self,
        _messages: Vec<Message>,
        _tools: Vec<ToolDefinition>,
        _context: LlmRequestContext,
    ) -> LlmStream {
        let events = self.turns.lock().unwrap().pop_front().unwrap_or_default();
        let delay = self.delay;
        Box::pin(stream::unfold(
            (events.into_iter(), delay),
            |(mut iter, delay)| async move {
                let ev = iter.next()?;
                tokio::time::sleep(delay).await;
                Some((ev, (iter, delay)))
            },
        ))
    }

    fn list_models(&self) -> ModelListFuture {
        Box::pin(async { Ok(vec![]) })
    }
}

struct SlowTool;

impl Tool for SlowTool {
    fn name(&self) -> &str {
        "slow_tool"
    }

    fn description(&self) -> &str {
        "test slow tool"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "value": { "type": "string" }
            }
        })
    }

    fn run(
        &self,
        args: serde_json::Value,
        _ctx: crate::agent::types::ToolCallContext,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = crate::agent::ToolResult> + Send + '_>>
    {
        let value = args
            .get("value")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        Box::pin(async move {
            tokio::time::sleep(std::time::Duration::from_millis(60)).await;
            crate::agent::ToolResult::ok_str(format!("slow:{value}"))
        })
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn make_tracker() -> std::sync::Arc<std::sync::Mutex<crate::agent::file_tracker::FileTracker>> {
    std::sync::Arc::new(std::sync::Mutex::new(
        crate::agent::file_tracker::FileTracker::new(),
    ))
}

fn make_log() -> std::sync::Arc<std::sync::Mutex<crate::agent::tool_output_log::ToolOutputLog>> {
    std::sync::Arc::new(std::sync::Mutex::new(
        crate::agent::tool_output_log::ToolOutputLog::new("test"),
    ))
}

fn make_executor() -> std::sync::Arc<DefaultToolExecutor> {
    std::sync::Arc::new(DefaultToolExecutor::new())
}

/// Run the agent loop with the given provider and hooks, and collect all emitted agent events.
async fn run_and_collect_with_config(
    provider: MockProvider,
    hooks: HashMap<crate::hooks::HookPoint, Vec<crate::hooks::HookConfig>>,
) -> Vec<AgentEvent> {
    let (tx, mut rx) = mpsc::unbounded_channel::<AppEvent>();
    let (_steering_tx, steering_rx) = mpsc::unbounded_channel();
    let (_cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
    let config = AgentLoopConfig {
        tools: HashMap::new(),
        file_tracker: make_tracker(),
        tool_output_log: make_log(),
        executor: make_executor(),
        session_events: vec![],
        current_model: "gpt-4o".to_string(),
        auto_compaction_enabled: true,
        manual_compaction_instructions: None,
        system_prompt: None,
        hooks,
        hook_ipc: crate::hooks::HookIpcPublisherHandle::disabled(),
        session_id: String::new(),
    };
    run_agent_loop(config, Arc::new(provider), tx, steering_rx, cancel_rx).await;
    let mut events = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        if let AppEvent::Agent(agent_ev) = ev {
            events.push(agent_ev);
        }
    }
    events
}

/// Run the agent loop with the given provider and collect all emitted agent events.
async fn run_and_collect(provider: MockProvider) -> Vec<AgentEvent> {
    run_and_collect_with_config(provider, HashMap::new()).await
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn agent_loop_single_text_turn() {
    let provider = MockProvider::new(vec![vec![
        LlmEvent::Token {
            text: "hello".to_string(),
            phase: AssistantPhase::Unknown,
        },
        LlmEvent::Done,
    ]]);
    let events = run_and_collect(provider).await;

    // First event must be TextToken("hello").
    assert!(
        matches!(&events[0], AgentEvent::TextToken { text, .. } if text == "hello"),
        "unexpected first event: {:?}",
        events[0]
    );
    // Last event must be Done.
    assert!(
        matches!(events.last().unwrap(), AgentEvent::Done),
        "expected Done as last event, got: {:?}",
        events.last()
    );
}

#[tokio::test]
async fn agent_loop_forwards_usage_event() {
    let provider = MockProvider::new(vec![vec![
        LlmEvent::Usage(UsageStats {
            input_tokens: Some(10),
            output_tokens: Some(5),
            total_tokens: Some(15),
            cached_tokens: None,
        }),
        LlmEvent::Token {
            text: "hello".to_string(),
            phase: AssistantPhase::Unknown,
        },
        LlmEvent::Done,
    ]]);
    let events = run_and_collect(provider).await;

    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::Usage(UsageStats {
                input_tokens: Some(10),
                output_tokens: Some(5),
                total_tokens: Some(15),
                ..
            })
        )),
        "expected forwarded usage event"
    );
}

#[tokio::test]
async fn agent_loop_tool_call_then_text() {
    // Turn 1: the model requests a tool call.
    // The tool is unknown so the loop returns an error result to the model.
    // Turn 2: the model gives a plain text answer.
    let provider = MockProvider::new(vec![
        vec![
            LlmEvent::ToolCall {
                id: "call_1".to_string(),
                name: "bash".to_string(),
                args: serde_json::json!({"command": "echo hi"}),
            },
            LlmEvent::Done,
        ],
        vec![
            LlmEvent::Token {
                text: "result".to_string(),
                phase: AssistantPhase::Final,
            },
            LlmEvent::Done,
        ],
    ]);
    let events = run_and_collect(provider).await;

    let has_tool_start = events
        .iter()
        .any(|e| matches!(e, AgentEvent::ToolCallStart { .. }));
    let has_tool_end = events
        .iter()
        .any(|e| matches!(e, AgentEvent::ToolCallEnd { .. }));
    let has_text = events
        .iter()
        .any(|e| matches!(e, AgentEvent::TextToken { text, .. } if text == "result"));
    let ends_done = matches!(events.last().unwrap(), AgentEvent::Done);

    assert!(has_tool_start, "expected ToolCallStart in events");
    assert!(has_tool_end, "expected ToolCallEnd in events");
    assert!(has_text, "expected TextToken('result') in events");
    assert!(ends_done, "expected Done as last event");
}

#[tokio::test]
async fn agent_loop_forwards_tool_intent_before_tool_start() {
    let provider = MockProvider::new(vec![vec![
        LlmEvent::ToolCallStart {
            id: "call_1".to_string(),
            name: "bash".to_string(),
        },
        LlmEvent::ToolCall {
            id: "call_1".to_string(),
            name: "bash".to_string(),
            args: serde_json::json!({"command": "echo hi"}),
        },
        LlmEvent::Done,
    ]]);

    let events = run_and_collect(provider).await;

    let intent_idx = events
        .iter()
        .position(|e| matches!(e, AgentEvent::ToolCallIntent { .. }))
        .expect("expected ToolCallIntent");
    let tool_start_idx = events
        .iter()
        .position(|e| matches!(e, AgentEvent::ToolCallStart { .. }))
        .expect("expected ToolCallStart");

    assert!(
        intent_idx < tool_start_idx,
        "expected ToolCallIntent before ToolCallStart"
    );
}

#[tokio::test]
async fn steering_during_tool_batch_finishes_batch_before_consuming_steering() {
    let provider = MockProvider::new(vec![
        vec![
            LlmEvent::ToolCall {
                id: "call_1".to_string(),
                name: "slow_tool".to_string(),
                args: serde_json::json!({"value": "a"}),
            },
            LlmEvent::ToolCall {
                id: "call_2".to_string(),
                name: "slow_tool".to_string(),
                args: serde_json::json!({"value": "b"}),
            },
            LlmEvent::Done,
        ],
        vec![
            LlmEvent::Token {
                text: "done".to_string(),
                phase: AssistantPhase::Final,
            },
            LlmEvent::Done,
        ],
    ]);

    let (tx, mut rx) = mpsc::unbounded_channel::<AppEvent>();
    let (steering_tx, steering_rx) = mpsc::unbounded_channel();
    let (_cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
    let mut tools: HashMap<String, Arc<dyn Tool>> = HashMap::new();
    tools.insert("slow_tool".to_string(), Arc::new(SlowTool));

    let config = AgentLoopConfig {
        tools,
        file_tracker: make_tracker(),
        tool_output_log: make_log(),
        executor: make_executor(),
        session_events: vec![],
        current_model: "gpt-4o".to_string(),
        auto_compaction_enabled: true,
        manual_compaction_instructions: None,
        system_prompt: None,
        hooks: HashMap::new(),
        hook_ipc: crate::hooks::HookIpcPublisherHandle::disabled(),
        session_id: String::new(),
    };

    let handle = tokio::spawn(async move {
        run_agent_loop(config, Arc::new(provider), tx, steering_rx, cancel_rx).await;
    });

    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    steering_tx
        .send("interrupt".to_string())
        .expect("queue steering");

    handle.await.expect("agent loop join");

    let mut events = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        if let AppEvent::Agent(agent_ev) = ev {
            events.push(agent_ev);
        }
    }

    let second_tool_ok = events.iter().any(|e| {
        matches!(
            e,
            AgentEvent::ToolCallEnd { id, result, .. }
            if id == "call_2" && !result.is_error && result.content.as_text().contains("slow:b")
        )
    });
    assert!(
        second_tool_ok,
        "expected second tool call to complete normally"
    );

    let turn_end_idx = events
        .iter()
        .position(|e| matches!(e, AgentEvent::TurnEnd))
        .expect("expected TurnEnd event");
    let steering_idx = events
        .iter()
        .position(|e| matches!(e, AgentEvent::SteeringConsumed { text } if text == "interrupt"))
        .expect("expected SteeringConsumed event");
    assert!(
        turn_end_idx < steering_idx,
        "expected steering to be consumed after TurnEnd"
    );

    assert!(matches!(events.last(), Some(AgentEvent::Done)));
}

#[tokio::test]
async fn cancellation_beats_steering_at_same_tool_boundary() {
    let provider = MockProvider::new(vec![vec![
        LlmEvent::ToolCall {
            id: "call_1".to_string(),
            name: "slow_tool".to_string(),
            args: serde_json::json!({"value": "a"}),
        },
        LlmEvent::ToolCall {
            id: "call_2".to_string(),
            name: "slow_tool".to_string(),
            args: serde_json::json!({"value": "b"}),
        },
        LlmEvent::Done,
    ]]);

    let (tx, mut rx) = mpsc::unbounded_channel::<AppEvent>();
    let (steering_tx, steering_rx) = mpsc::unbounded_channel();
    let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
    let mut tools: HashMap<String, Arc<dyn Tool>> = HashMap::new();
    tools.insert("slow_tool".to_string(), Arc::new(SlowTool));

    let config = AgentLoopConfig {
        tools,
        file_tracker: make_tracker(),
        tool_output_log: make_log(),
        executor: make_executor(),
        session_events: vec![],
        current_model: "gpt-4o".to_string(),
        auto_compaction_enabled: true,
        manual_compaction_instructions: None,
        system_prompt: None,
        hooks: HashMap::new(),
        hook_ipc: crate::hooks::HookIpcPublisherHandle::disabled(),
        session_id: String::new(),
    };

    let handle = tokio::spawn(async move {
        run_agent_loop(config, Arc::new(provider), tx, steering_rx, cancel_rx).await;
    });

    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    steering_tx
        .send("interrupt".to_string())
        .expect("queue steering");
    cancel_tx.send(true).expect("queue cancellation");

    handle.await.expect("agent loop join");

    let mut events = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        if let AppEvent::Agent(agent_ev) = ev {
            events.push(agent_ev);
        }
    }

    let interrupted_second = events.iter().any(|e| {
        matches!(
            e,
            AgentEvent::ToolCallEnd { id, result, .. }
            if id == "call_2" && result.is_error && result.content.as_text().contains("Interrupted by user")
        )
    });
    assert!(
        interrupted_second,
        "expected second tool call to be interrupted"
    );

    assert!(
        !events.iter().any(|e| matches!(
            e,
            AgentEvent::SteeringConsumed { text } if text == "interrupt"
        )),
        "expected cancellation to win before steering is consumed"
    );

    assert!(matches!(events.last(), Some(AgentEvent::TurnEnd)));
}

#[tokio::test]
async fn steering_after_streamed_text_is_consumed_after_turn_end() {
    let provider = DelayedMockProvider::new(
        vec![
            vec![
                LlmEvent::ThinkingToken("thinking".to_string()),
                LlmEvent::Token {
                    text: "answer".to_string(),
                    phase: AssistantPhase::Final,
                },
                LlmEvent::Done,
            ],
            vec![
                LlmEvent::Token {
                    text: "follow-up".to_string(),
                    phase: AssistantPhase::Final,
                },
                LlmEvent::Done,
            ],
        ],
        std::time::Duration::from_millis(20),
    );

    let (tx, mut rx) = mpsc::unbounded_channel::<AppEvent>();
    let (steering_tx, steering_rx) = mpsc::unbounded_channel();
    let (_cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
    let config = AgentLoopConfig {
        tools: HashMap::new(),
        file_tracker: make_tracker(),
        tool_output_log: make_log(),
        executor: make_executor(),
        session_events: vec![],
        current_model: "gpt-4o".to_string(),
        auto_compaction_enabled: true,
        manual_compaction_instructions: None,
        system_prompt: None,
        hooks: HashMap::new(),
        hook_ipc: crate::hooks::HookIpcPublisherHandle::disabled(),
        session_id: String::new(),
    };

    let handle = tokio::spawn(async move {
        run_agent_loop(config, Arc::new(provider), tx, steering_rx, cancel_rx).await;
    });

    tokio::time::sleep(std::time::Duration::from_millis(1)).await;
    steering_tx
        .send("test".to_string())
        .expect("queue steering");

    handle.await.expect("agent loop join");

    let mut events = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        if let AppEvent::Agent(agent_ev) = ev {
            events.push(agent_ev);
        }
    }

    let answer_idx = events
        .iter()
        .rposition(|e| matches!(e, AgentEvent::TextToken { text, .. } if text == "answer"))
        .expect("expected answer token");
    let first_turn_end_idx = events
        .iter()
        .position(|e| matches!(e, AgentEvent::TurnEnd))
        .expect("expected first TurnEnd");
    let steering_idx = events
        .iter()
        .position(|e| matches!(e, AgentEvent::SteeringConsumed { text } if text == "test"))
        .expect("expected SteeringConsumed event");

    assert!(
        answer_idx < first_turn_end_idx,
        "expected answer before TurnEnd"
    );
    assert!(
        first_turn_end_idx < steering_idx,
        "expected steering to be consumed after TurnEnd"
    );
    assert!(matches!(events.last(), Some(AgentEvent::Done)));
}

#[tokio::test]
async fn agent_loop_stream_error_is_reported() {
    use crate::llm::ProviderError;
    let err = ProviderError::other("test", "boom");
    let provider = MockProvider::new(vec![vec![LlmEvent::Error(err.clone())]]);
    let events = run_and_collect(provider).await;

    assert!(
        matches!(events.last().unwrap(), AgentEvent::Error(e) if e.message == "boom"),
        "expected Error with 'boom' message as last event, got: {:?}",
        events.last()
    );
}

#[tokio::test]
async fn agent_loop_before_hook_blocks_tool() {
    // Turn 1: model requests a tool call; `before_tool_call` returns false.
    // Turn 2: model gives a plain text answer after seeing the error result.
    let provider = MockProvider::new(vec![
        vec![
            LlmEvent::ToolCall {
                id: "call_1".to_string(),
                name: "bash".to_string(),
                args: serde_json::json!({"command": "echo hi"}),
            },
            LlmEvent::Done,
        ],
        vec![
            LlmEvent::Token {
                text: "ok".to_string(),
                phase: AssistantPhase::Final,
            },
            LlmEvent::Done,
        ],
    ]);

    let (tx, mut rx) = mpsc::unbounded_channel::<AppEvent>();
    let config = AgentLoopConfig {
        tools: HashMap::new(),
        file_tracker: make_tracker(),
        tool_output_log: make_log(),
        executor: std::sync::Arc::new(crate::agent::types::DefaultToolExecutor::with_hooks(
            Some(Box::new(|_name, _args| false)), // block everything
            None,
        )),
        session_events: vec![],
        current_model: "gpt-4o".to_string(),
        auto_compaction_enabled: true,
        manual_compaction_instructions: None,
        system_prompt: None,
        hooks: HashMap::new(),
        hook_ipc: crate::hooks::HookIpcPublisherHandle::disabled(),
        session_id: String::new(),
    };
    let (_steering_tx, steering_rx) = mpsc::unbounded_channel();
    let (_cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
    run_agent_loop(config, Arc::new(provider), tx, steering_rx, cancel_rx).await;

    let mut events = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        if let AppEvent::Agent(agent_ev) = ev {
            events.push(agent_ev);
        }
    }

    let has_blocked_result = events
        .iter()
        .any(|ev| matches!(ev, AgentEvent::ToolCallEnd { result, .. } if result.is_error));
    assert!(
        has_blocked_result,
        "expected a blocked (is_error) ToolCallEnd"
    );

    // Loop should still complete (not hang or error out).
    assert!(
        matches!(events.last().unwrap(), AgentEvent::Done),
        "expected Done after blocked tool call"
    );
}

#[test]
fn test_read_agents_md() {
    use std::fs;

    use tempfile::tempdir;

    use crate::agent::system_prompt::AgentsKind;

    // Create a temporary directory structure.
    let temp_home = tempdir().unwrap();
    let home_path = temp_home.path();
    let temp_working = tempdir().unwrap();
    let working_path = temp_working.path();

    // Simulate ~/.xi/AGENTS.md
    let xi_agents_md = home_path.join(".xi/AGENTS.md");
    fs::create_dir_all(xi_agents_md.parent().unwrap()).unwrap();
    fs::write(&xi_agents_md, "Global agents configuration\n").unwrap();

    // Simulate AGENTS.md at cwd.
    let cwd_agents_md = working_path.join("AGENTS.md");
    fs::write(&cwd_agents_md, "Local agents configuration\n").unwrap();

    // Mock the home and current directory paths.
    let cwd = working_path.display().to_string();
    let entries = crate::agent::system_prompt::read_agents_md(&cwd, Some(home_path));

    assert_eq!(entries.len(), 2);

    // Global entry comes first.
    assert_eq!(entries[0].kind, AgentsKind::Global);
    assert!(entries[0].content.contains("Global agents configuration"));

    // Local entry comes second.
    assert_eq!(entries[1].kind, AgentsKind::Local);
    assert!(entries[1].content.contains("Local agents configuration"));

    // Paths are preserved.
    assert!(entries[0].path.to_string_lossy().contains(".xi/AGENTS.md"));
    assert!(entries[1].path.to_string_lossy().contains("AGENTS.md"));

    temp_home.close().unwrap();
    temp_working.close().unwrap();
}

#[test]
fn test_read_agents_md_falls_back_to_dot_agents() {
    use std::fs;

    use tempfile::tempdir;

    use crate::agent::system_prompt::AgentsKind;

    let temp_home = tempdir().unwrap();
    let home_path = temp_home.path();
    let temp_working = tempdir().unwrap();
    let working_path = temp_working.path();

    // Only ~/.agents/AGENTS.md exists (no ~/.xi/AGENTS.md).
    let agents_md = home_path.join(".agents/AGENTS.md");
    fs::create_dir_all(agents_md.parent().unwrap()).unwrap();
    fs::write(&agents_md, "Global .agents config\n").unwrap();

    let cwd = working_path.display().to_string();
    let entries = crate::agent::system_prompt::read_agents_md(&cwd, Some(home_path));

    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].kind, AgentsKind::Global);
    assert!(entries[0].content.contains("Global .agents config"));
    assert!(
        entries[0]
            .path
            .to_string_lossy()
            .contains(".agents/AGENTS.md")
    );

    temp_home.close().unwrap();
    temp_working.close().unwrap();
}

#[test]
fn test_read_agents_md_xi_overrides_agents_at_same_level() {
    use std::fs;

    use tempfile::tempdir;

    use crate::agent::system_prompt::AgentsKind;

    let temp_home = tempdir().unwrap();
    let home_path = temp_home.path();

    // Both ~/.xi/AGENTS.md and ~/.agents/AGENTS.md exist.
    let xi_agents_md = home_path.join(".xi/AGENTS.md");
    fs::create_dir_all(xi_agents_md.parent().unwrap()).unwrap();
    fs::write(&xi_agents_md, ".xi content\n").unwrap();

    let agents_md = home_path.join(".agents/AGENTS.md");
    fs::create_dir_all(agents_md.parent().unwrap()).unwrap();
    fs::write(&agents_md, ".agents content\n").unwrap();

    let cwd = tempdir().unwrap();
    let entries = crate::agent::system_prompt::read_agents_md(
        &cwd.path().display().to_string(),
        Some(home_path),
    );

    // .xi takes priority; only one entry, and it's from .xi.
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].kind, AgentsKind::Global);
    assert!(entries[0].content.contains(".xi content"));
    assert!(!entries[0].content.contains(".agents content"));
    assert!(entries[0].path.to_string_lossy().contains(".xi/AGENTS.md"));

    temp_home.close().unwrap();
    cwd.close().unwrap();
}

#[test]
fn test_read_agents_md_dot_xi_in_cwd_walk() {
    use std::fs;

    use tempfile::tempdir;

    use crate::agent::system_prompt::AgentsKind;

    let temp_home = tempdir().unwrap();
    let home_path = temp_home.path();

    // No global file.
    let working = tempdir().unwrap();
    let working_path = working.path();

    // .xi/AGENTS.md inside cwd (prioritised over bare AGENTS.md).
    let xi_path = working_path.join(".xi/AGENTS.md");
    fs::create_dir_all(xi_path.parent().unwrap()).unwrap();
    fs::write(&xi_path, ".xi project config\n").unwrap();

    // Also create bare AGENTS.md to confirm it's NOT picked up.
    fs::write(working_path.join("AGENTS.md"), "bare config\n").unwrap();

    let cwd = working_path.display().to_string();
    let entries = crate::agent::system_prompt::read_agents_md(&cwd, Some(home_path));

    // Only one entry (.xi takes priority over bare).
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].kind, AgentsKind::Local);
    assert!(entries[0].content.contains(".xi project config"));
    assert!(!entries[0].content.contains("bare config"));
    assert!(entries[0].path.to_string_lossy().contains(".xi/AGENTS.md"));

    temp_home.close().unwrap();
    working.close().unwrap();
}

#[test]
fn test_read_agents_md_from_nested_cwd_includes_parent_chain_in_order() {
    use std::fs;

    use tempfile::tempdir;

    use crate::agent::system_prompt::AgentsKind;

    let temp_home = tempdir().unwrap();
    let home_path = temp_home.path();

    // Simulate ~/.xi/AGENTS.md
    let xi_agents_md = home_path.join(".xi/AGENTS.md");
    fs::create_dir_all(xi_agents_md.parent().unwrap()).unwrap();
    fs::write(&xi_agents_md, "Global config\n").unwrap();

    // Build nested workspace: <root>/project/subdir
    let root = tempdir().unwrap();
    let project = root.path().join("project");
    let subdir = project.join("subdir");
    fs::create_dir_all(&subdir).unwrap();

    fs::write(project.join("AGENTS.md"), "Project-level config\n").unwrap();
    fs::write(subdir.join("AGENTS.md"), "Subdir-level config\n").unwrap();

    let cwd = subdir.display().to_string();
    let entries = crate::agent::system_prompt::read_agents_md(&cwd, Some(home_path));

    assert_eq!(entries.len(), 3);

    // Order: global, cwd, parent
    assert_eq!(entries[0].kind, AgentsKind::Global);
    assert!(entries[0].content.contains("Global config"));

    assert_eq!(entries[1].kind, AgentsKind::Local);
    assert!(entries[1].content.contains("Subdir-level config"));

    assert_eq!(entries[2].kind, AgentsKind::Local);
    assert!(entries[2].content.contains("Project-level config"));

    temp_home.close().unwrap();
    root.close().unwrap();
}

// ── ask_user integration tests ────────────────────────────────────────────────

#[tokio::test]
async fn agent_loop_ask_user_no_options_completes_loop() {
    use tokio::sync::mpsc as tmspc;

    let (app_tx, mut app_rx) = tmspc::unbounded_channel::<AppEvent>();

    let mut tools: HashMap<String, Arc<dyn Tool>> = HashMap::new();
    tools.insert(
        "ask_user".to_string(),
        Arc::new(AskUserTool::new(Some(app_tx), None)),
    );

    // Turn 1: LLM asks a freeform question (no options).
    // Turn 2: LLM gives the final answer after receiving the user's reply.
    let provider = MockProvider::new(vec![
        vec![
            LlmEvent::ToolCall {
                id: "call_1".to_string(),
                name: "ask_user".to_string(),
                args: serde_json::json!({ "question": "What is your name?" }),
            },
            LlmEvent::Done,
        ],
        vec![
            LlmEvent::Token {
                text: "Nice to meet you!".to_string(),
                phase: AssistantPhase::Final,
            },
            LlmEvent::Done,
        ],
    ]);

    let (tx, mut rx) = mpsc::unbounded_channel::<AppEvent>();
    let (_steering_tx, steering_rx) = mpsc::unbounded_channel();
    let (_cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
    let config = AgentLoopConfig {
        tools,
        file_tracker: make_tracker(),
        tool_output_log: make_log(),
        executor: make_executor(),
        session_events: vec![],
        current_model: "gpt-4o".to_string(),
        auto_compaction_enabled: true,
        manual_compaction_instructions: None,
        system_prompt: None,
        hooks: HashMap::new(),
        hook_ipc: crate::hooks::HookIpcPublisherHandle::disabled(),
        session_id: String::new(),
    };

    let handle = tokio::spawn(async move {
        run_agent_loop(config, Arc::new(provider), tx, steering_rx, cancel_rx).await;
    });

    // Simulate the UI: receive the ask request and reply with a freeform answer.
    let req = loop {
        let ev = app_rx
            .recv()
            .await
            .expect("agent should send app events while ask_user is pending");
        if let AppEvent::AskUser(req) = ev {
            break req;
        }
    };
    assert!(req.options.is_empty(), "expected no options");
    req.reply
        .send(AskUserResponse::Answer("Alice".to_string()))
        .expect("reply channel should be open");

    handle.await.expect("agent loop task should complete");

    let mut events = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        if let AppEvent::Agent(agent_ev) = ev {
            events.push(agent_ev);
        }
    }

    assert!(
        matches!(events.last(), Some(AgentEvent::Done)),
        "expected Done as last event, got: {:?}",
        events.last()
    );
    assert!(
        events.iter().any(
            |e| matches!(e, AgentEvent::TextToken { text, .. } if text == "Nice to meet you!")
        ),
        "expected final text token after ask_user answer"
    );
}

// ── Cancellation tests ────────────────────────────────────────────────────────

/// A loop started with cancel already set to true must return immediately
/// without making any LLM call.
#[tokio::test]
async fn agent_loop_pre_cancelled_exits_immediately() {
    // Provider would panic if called — any invocation means the test fails.
    struct PanicProvider;
    impl LlmProvider for PanicProvider {
        fn stream_chat(&self, _: Vec<Message>, _: LlmRequestContext) -> LlmStream {
            panic!("LLM should not be called when pre-cancelled")
        }
        fn stream_chat_with_tools(
            &self,
            _: Vec<Message>,
            _: Vec<ToolDefinition>,
            _: LlmRequestContext,
        ) -> LlmStream {
            panic!("LLM should not be called when pre-cancelled")
        }
        fn list_models(&self) -> ModelListFuture {
            Box::pin(async { Ok(vec![]) })
        }
    }

    let (tx, mut rx) = mpsc::unbounded_channel::<AppEvent>();
    let (_steering_tx, steering_rx) = mpsc::unbounded_channel();
    // Pre-cancel: send true before the loop even starts.
    let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(true);
    drop(cancel_tx); // sender no longer needed

    let config = AgentLoopConfig {
        tools: HashMap::new(),
        file_tracker: make_tracker(),
        tool_output_log: make_log(),
        executor: make_executor(),
        session_events: vec![],
        current_model: "gpt-4o".to_string(),
        auto_compaction_enabled: true,
        manual_compaction_instructions: None,
        system_prompt: None,
        hooks: HashMap::new(),
        hook_ipc: crate::hooks::HookIpcPublisherHandle::disabled(),
        session_id: String::new(),
    };

    run_agent_loop(config, Arc::new(PanicProvider), tx, steering_rx, cancel_rx).await;

    // No events should have been emitted.
    let mut events = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        if let AppEvent::Agent(agent_ev) = ev {
            events.push(agent_ev);
        }
    }
    assert!(
        events.is_empty(),
        "expected no events for pre-cancelled loop, got: {events:?}"
    );
}

/// Cancelling after a tool call completes stops the loop before the next LLM
/// turn — the first tool call's result is delivered but no second turn starts.
#[tokio::test]
async fn agent_loop_cancel_after_tool_call_stops_before_next_turn() {
    // Turn 1: one tool call.
    // Turn 2 (would be after tool result): plain text — must never be reached.
    let provider = MockProvider::new(vec![
        vec![
            LlmEvent::ToolCall {
                id: "call_1".to_string(),
                name: "slow_tool".to_string(),
                args: serde_json::json!({"value": "x"}),
            },
            LlmEvent::Done,
        ],
        vec![
            LlmEvent::Token {
                text: "second-turn".to_string(),
                phase: AssistantPhase::Final,
            },
            LlmEvent::Done,
        ],
    ]);

    let (tx, mut rx) = mpsc::unbounded_channel::<AppEvent>();
    let (_steering_tx, steering_rx) = mpsc::unbounded_channel();
    let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);

    let mut tools: HashMap<String, Arc<dyn Tool>> = HashMap::new();
    tools.insert("slow_tool".to_string(), Arc::new(SlowTool));

    let config = AgentLoopConfig {
        tools,
        file_tracker: make_tracker(),
        tool_output_log: make_log(),
        executor: std::sync::Arc::new(crate::agent::types::DefaultToolExecutor::with_hooks(
            None,
            // Cancel via the watch channel as soon as the tool call finishes.
            Some(Box::new(move |_name, _result| {
                let _ = cancel_tx.send(true);
                None
            })),
        )),
        session_events: vec![],
        current_model: "gpt-4o".to_string(),
        auto_compaction_enabled: true,
        manual_compaction_instructions: None,
        system_prompt: None,
        hooks: HashMap::new(),
        hook_ipc: crate::hooks::HookIpcPublisherHandle::disabled(),
        session_id: String::new(),
    };

    run_agent_loop(config, Arc::new(provider), tx, steering_rx, cancel_rx).await;

    let mut events = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        if let AppEvent::Agent(agent_ev) = ev {
            events.push(agent_ev);
        }
    }

    // The first tool call must have completed.
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::ToolCallEnd { id, .. } if id == "call_1")),
        "expected ToolCallEnd for call_1"
    );
    // The second LLM turn must not have produced any text.
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::TextToken { text, .. } if text == "second-turn")),
        "second turn should not have been reached after cancellation"
    );
}

#[tokio::test]
async fn post_turn_hook_fires_after_final_answer() {
    use crate::hooks::{HookConfig, HookPoint};
    use std::collections::HashMap;

    let hook_path =
        std::env::temp_dir().join(format!("xi-hook-test-post-turn-{}.txt", std::process::id()));

    let mut hooks = HashMap::new();
    hooks.insert(
        HookPoint::PostTurn,
        vec![HookConfig {
            command: Some(post_turn_test_hook_program().into()),
            args: post_turn_test_hook_args(&hook_path),
            ..Default::default()
        }],
    );

    let provider = MockProvider::new(vec![vec![
        LlmEvent::Token {
            text: "done".to_string(),
            phase: AssistantPhase::Unknown,
        },
        LlmEvent::Done,
    ]]);

    let _events = run_and_collect_with_config(provider, hooks).await;

    let content = std::fs::read_to_string(&hook_path).unwrap_or_default();
    assert!(
        content.contains("HOOK OK"),
        "post_turn hook did not fire: content={content:?}"
    );
    let _ = std::fs::remove_file(&hook_path);
}

#[cfg(unix)]
fn post_turn_test_hook_program() -> &'static str {
    "sh"
}

#[cfg(unix)]
fn post_turn_test_hook_args(path: &std::path::Path) -> Vec<String> {
    vec![
        "-c".into(),
        format!("printf 'HOOK OK' > '{}'", path.display()),
    ]
}

#[cfg(windows)]
fn post_turn_test_hook_program() -> &'static str {
    "powershell.exe"
}

#[cfg(windows)]
fn post_turn_test_hook_args(path: &std::path::Path) -> Vec<String> {
    let escaped = path.display().to_string().replace('\'', "''");
    vec![
        "-NoProfile".into(),
        "-Command".into(),
        format!("Set-Content -Path '{escaped}' -Value 'HOOK OK'"),
    ]
}

// ── build_sorted_tool_defs ────────────────────────────────────────────────────

/// Helper for tests: build a `ToolRegistry` from a `Vec` of names in a known
/// insertion order.  The resulting `HashMap` will iterate in an order that
/// depends on its internal state, so we use this to verify that
/// `build_sorted_tool_defs` sorts alphabetically regardless.
#[cfg(test)]
fn registry_from_names(names: &[&'static str]) -> crate::agent::types::ToolRegistry {
    use crate::agent::types::Tool;
    use std::sync::Arc;

    struct NamedTool(&'static str);
    impl Tool for NamedTool {
        fn name(&self) -> &str {
            self.0
        }
        fn description(&self) -> &str {
            ""
        }
        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({})
        }
        fn run(
            &self,
            _args: serde_json::Value,
            _ctx: crate::agent::types::ToolCallContext,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = crate::agent::types::ToolResult> + Send + '_>,
        > {
            Box::pin(async { crate::agent::types::ToolResult::ok_str("") })
        }
    }

    let mut map = std::collections::HashMap::new();
    for &name in names {
        map.insert(name.to_string(), Arc::new(NamedTool(name)) as Arc<dyn Tool>);
    }
    map
}

#[test]
fn tool_defs_are_sorted_alphabetically() {
    use crate::agent::build_sorted_tool_defs;
    use std::collections::HashSet;

    // Run with several different input orderings.  Without the sort in
    // build_sorted_tool_defs, the output order depends on HashMap iteration
    // which varies with capacity and key hashes.  With the sort it is always
    // alphabetical.
    for input in [
        vec!["zebra", "alpha", "beta", "gamma"],
        vec!["delta", "charlie", "bravo", "alpha"],
        vec!["bash", "exec", "python", "read_file", "ask_user"],
    ] {
        let registry = registry_from_names(&input);
        let defs = build_sorted_tool_defs(&registry);
        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();

        // Verify sorted.
        for pair in names.windows(2) {
            assert!(pair[0] < pair[1], "{pair:?} not sorted");
        }
        // Verify no tools lost.
        let expected: HashSet<&str> = input.iter().copied().collect();
        let got: HashSet<&str> = names.iter().copied().collect();
        assert_eq!(got, expected);
    }
}
