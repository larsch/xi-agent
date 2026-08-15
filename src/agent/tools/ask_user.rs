use std::pin::Pin;
use std::sync::{Arc, Mutex};

use serde_json::Value;
use tokio::sync::{Semaphore, oneshot};

use crate::agent::file_tracker::FileTracker;
use crate::agent::types::{AskRequest, AskUserOption, AskUserResponse, Tool, ToolResult};
use crate::app_event::{AppEvent, AppEventTx};

pub struct AskUserTool {
    tx: Option<AppEventTx>,
    file_tracker: Option<Arc<Mutex<FileTracker>>>,
    pending: Arc<Semaphore>,
}

impl AskUserTool {
    pub fn new(tx: Option<AppEventTx>, file_tracker: Option<Arc<Mutex<FileTracker>>>) -> Self {
        Self {
            tx,
            file_tracker,
            pending: Arc::new(Semaphore::new(1)),
        }
    }
}

#[derive(serde::Deserialize)]
struct AskUserArgs {
    question: String,
    context: Option<String>,
    /// Kept as raw Value because option items can be strings or objects, and
    /// some models emit a JSON-encoded string instead of an array.  The
    /// existing `parse_options` helper handles all of these cases.
    options: Option<Value>,
    #[serde(rename = "allowMultiple", default)]
    allow_multiple: bool,
    #[serde(rename = "allowFreeform", default = "default_allow_freeform")]
    allow_freeform: bool,
}

fn default_allow_freeform() -> bool {
    true
}

impl Tool for AskUserTool {
    fn name(&self) -> &str {
        "ask_user"
    }

    fn description(&self) -> &str {
        "Ask the user a question with optional multiple-choice answers. Use this only when you need user input to proceed."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "question": {
                    "type": "string",
                    "description": "The question to ask the user"
                },
                "context": {
                    "type": "string",
                    "description": "Relevant context summary shown before the question (optional)"
                },
                "options": {
                    "type": "array",
                    "description": "Optional choice list. Each option can be a string or object with title/description.",
                    "items": {
                        "anyOf": [
                            { "type": "string" },
                            {
                                "type": "object",
                                "properties": {
                                    "title": { "type": "string" },
                                    "description": { "type": "string" }
                                },
                                "required": ["title"]
                            }
                        ]
                    }
                },
                "allowMultiple": {
                    "type": "boolean",
                    "description": "Whether multiple options may be selected (currently ignored in xi-agent UI)"
                },
                "allowFreeform": {
                    "type": "boolean",
                    "description": "Whether to allow freeform input in addition to options"
                }
            },
            "required": ["question"]
        })
    }

    fn streaming_field(&self) -> Option<&'static str> {
        Some("question")
    }

    fn run(
        &self,
        args: Value,
        ctx: crate::agent::types::ToolCallContext,
    ) -> Pin<Box<dyn std::future::Future<Output = ToolResult> + Send + '_>> {
        Box::pin(async move {
            let Ok(_permit) = Arc::clone(&self.pending).try_acquire_owned() else {
                return ToolResult::err(
                    "ask_user request rejected: another question is already pending. ".to_string()
                        + "Wait for its result, then repeat this question if it is still needed.",
                );
            };

            let Some(tx) = &self.tx else {
                return ToolResult::err("ask_user is unavailable in non-interactive mode");
            };

            let AskUserArgs {
                question,
                context,
                options,
                allow_multiple,
                allow_freeform,
            } = match super::parse_args(args) {
                Ok(a) => a,
                Err(e) => return *e,
            };

            let question = question.trim().to_string();
            if question.is_empty() {
                return ToolResult::err("question must not be empty");
            }

            let context = context
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(ToOwned::to_owned);

            let options = parse_options(options.as_ref());

            let (reply_tx, reply_rx) = oneshot::channel();

            let request = AskRequest {
                question: question.clone(),
                context,
                options,
                allow_multiple,
                allow_freeform,
                reply: reply_tx,
            };

            // Fire the on_ask_user hook before blocking on user input.
            ctx.hook_ipc.publish(
                &ctx.session_id,
                crate::hooks::HookPoint::OnAskUser,
                None,
                crate::hooks::ipc_ask_user_payload(&question),
            );
            crate::hooks::maybe_run_hook(
                &ctx.hooks,
                crate::hooks::HookPoint::OnAskUser,
                &ctx.session_id,
                Some(crate::hooks::on_ask_user_json(&question)),
                None,
            )
            .await;

            if tx.send(AppEvent::AskUser(request)).is_err() {
                return ToolResult::err("ask_user failed: UI channel closed");
            }

            // Refresh baselines before blocking on user input.  While the
            // agent is paused waiting for an answer the user may edit files,
            // and we want to detect those changes — but we do NOT want to
            // report changes the agent itself caused before calling ask_user.
            if let Some(tracker) = &self.file_tracker {
                tracker.lock().unwrap().refresh_baselines();
            }

            match reply_rx.await {
                Ok(AskUserResponse::Answer(answer)) => ToolResult::ok_str(answer),
                Ok(AskUserResponse::Cancelled) => ToolResult::err("ask_user cancelled by user"),
                Err(_) => ToolResult::err("ask_user failed: reply channel closed"),
            }
        })
    }
}

fn parse_options(raw: Option<&Value>) -> Vec<AskUserOption> {
    let Some(value) = raw else {
        return Vec::new();
    };

    let normalized = match value {
        Value::Array(items) => Some(items.clone()),
        // Some models (notably local ones) occasionally emit a JSON-encoded
        // array as a string for complex tool args. Be lenient and parse it.
        Value::String(s) => serde_json::from_str::<Value>(s).ok().and_then(|v| match v {
            Value::Array(items) => Some(items),
            _ => None,
        }),
        _ => None,
    };

    let Some(items) = normalized else {
        return Vec::new();
    };

    items
        .iter()
        .filter_map(|item| match item {
            Value::String(s) => {
                let title = s.trim();
                if title.is_empty() {
                    None
                } else {
                    Some(AskUserOption {
                        title: title.to_string(),
                        description: None,
                    })
                }
            }
            Value::Object(obj) => {
                let title = obj.get("title").and_then(Value::as_str)?.trim();
                if title.is_empty() {
                    return None;
                }
                let description = obj
                    .get("description")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(ToOwned::to_owned);
                Some(AskUserOption {
                    title: title.to_string(),
                    description,
                })
            }
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::types::ToolCallContext;
    use crate::app_event::AppEvent;
    use tokio::sync::mpsc;

    #[tokio::test]
    async fn rejects_overlapping_questions_and_releases_permit() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let tool = AskUserTool::new(Some(tx), None);
        let args = serde_json::json!({"question": "Continue?"});

        let first = tool.run(args.clone(), ToolCallContext::noop("first"));
        tokio::pin!(first);
        let first_request = tokio::select! {
            result = &mut first => panic!("first question completed early: {:?}", result.content.as_text()),
            event = rx.recv() => match event.expect("first request") {
                AppEvent::AskUser(request) => request,
                other => panic!("unexpected event: {other:?}"),
            },
        };

        let duplicate = tool
            .run(args.clone(), ToolCallContext::noop("duplicate"))
            .await;
        assert!(duplicate.is_error);
        assert_eq!(
            duplicate.content.as_text(),
            "ask_user request rejected: another question is already pending. Wait for its result, then repeat this question if it is still needed."
        );

        first_request
            .reply
            .send(AskUserResponse::Answer("yes".to_string()))
            .expect("first reply");
        assert_eq!(first.await.content.as_text(), "yes");

        let second = tool.run(args, ToolCallContext::noop("second"));
        tokio::pin!(second);
        let second_request = tokio::select! {
            result = &mut second => panic!("second question completed early: {:?}", result.content.as_text()),
            event = rx.recv() => match event.expect("second request") {
                AppEvent::AskUser(request) => request,
                other => panic!("unexpected event: {other:?}"),
            },
        };
        second_request
            .reply
            .send(AskUserResponse::Cancelled)
            .expect("second reply");
        assert!(second.await.is_error);
    }
}
