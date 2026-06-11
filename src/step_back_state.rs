/// State for step-back navigation through conversation history.
///
/// Steps back to boundaries where user input was provided: `UserMessage`
/// events (turn boundaries) and `ToolResult` events for `ask_user`
/// (in-turn question answers).
///
/// Both fields are always set and cleared together, which is the invariant
/// this struct enforces.
#[derive(Default)]
pub(crate) struct StepBackState {
    /// Index into the session event log pointing at the boundary event
    /// (`UserMessage` or `ToolResult { name: "ask_user" }`) that serves as
    /// the resubmission point.  `None` when not stepping.
    pub(crate) cursor: Option<usize>,
    /// Input field content saved when stepping began, restored on cancel.
    pub(crate) saved_input: Option<String>,
}

impl StepBackState {
    pub(crate) fn is_stepping(&self) -> bool {
        self.cursor.is_some()
    }

    /// Save the current textarea content so it can be restored on cancel.
    pub(crate) fn save_input(&mut self, text: String) {
        self.saved_input = Some(text);
    }

    /// Clear all step state.  Returns the saved input if present.
    pub(crate) fn cancel(&mut self) -> Option<String> {
        self.cursor = None;
        self.saved_input.take()
    }

    /// Clear all step state without returning the saved input (used on commit).
    pub(crate) fn clear(&mut self) {
        self.cursor = None;
        self.saved_input = None;
    }
}
