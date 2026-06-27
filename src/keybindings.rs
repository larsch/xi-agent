use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum KeyBindingId {
    ShowHelp,
    Quit,
    QuitIfInputEmpty,
    ToggleInfo,
    ToggleInfoAlt,
    CopyLastAssistantResponse,
    ToggleFullOutput,
    ResumeLatestSession,
    CycleShell,
    EditProvider,
    RemoveProvider,
    EnterShellMode,
    StepBack,
    ScrollPageUp,
    Submit,
    InsertNewline,
    ApplyCompletion,
    Cancel,
    ExitShellOnEmptyBackspace,
    SelectionUp,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BindingContext {
    Global,
    Chat,
    Shell,
    Selection,
    ProviderPicker,
    Mouse,
}

impl BindingContext {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Global => "Global",
            Self::Chat => "Chat",
            Self::Shell => "Shell",
            Self::Selection => "Selection",
            Self::ProviderPicker => "Provider picker",
            Self::Mouse => "Mouse",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct KeyBinding {
    pub(crate) id: Option<KeyBindingId>,
    pub(crate) shortcut: &'static str,
    pub(crate) context: BindingContext,
    pub(crate) description: &'static str,
}

pub(crate) const KEYBINDINGS: &[KeyBinding] = &[
    KeyBinding {
        id: Some(KeyBindingId::ShowHelp),
        shortcut: "F1",
        context: BindingContext::Global,
        description: "Show keyboard shortcuts",
    },
    KeyBinding {
        id: Some(KeyBindingId::Cancel),
        shortcut: "Esc",
        context: BindingContext::Global,
        description: "Cancel or close the current context; abort the agent loop if running",
    },
    KeyBinding {
        id: Some(KeyBindingId::Quit),
        shortcut: "Ctrl+C",
        context: BindingContext::Global,
        description: "Quit, or leave shell mode if active",
    },
    KeyBinding {
        id: Some(KeyBindingId::QuitIfInputEmpty),
        shortcut: "Ctrl+D",
        context: BindingContext::Global,
        description: "Quit when input is empty, or leave shell mode if shell input is empty",
    },
    KeyBinding {
        id: Some(KeyBindingId::ToggleInfo),
        shortcut: "Ctrl+I",
        context: BindingContext::Global,
        description: "Toggle provider/model info bar",
    },
    KeyBinding {
        id: Some(KeyBindingId::ToggleInfoAlt),
        shortcut: "Alt+S",
        context: BindingContext::Global,
        description: "Toggle provider/model info bar",
    },
    KeyBinding {
        id: Some(KeyBindingId::CopyLastAssistantResponse),
        shortcut: "Alt+C",
        context: BindingContext::Global,
        description: "Copy the last assistant response",
    },
    KeyBinding {
        id: Some(KeyBindingId::ToggleFullOutput),
        shortcut: "Ctrl+F",
        context: BindingContext::Global,
        description: "Toggle full tool output in the log",
    },
    KeyBinding {
        id: Some(KeyBindingId::ResumeLatestSession),
        shortcut: "Ctrl+R",
        context: BindingContext::Chat,
        description: "Resume the latest session for the current folder",
    },
    KeyBinding {
        id: Some(KeyBindingId::EnterShellMode),
        shortcut: "!",
        context: BindingContext::Chat,
        description: "Enter shell mode when chat input is empty",
    },
    KeyBinding {
        id: Some(KeyBindingId::SelectionUp),
        shortcut: "Up / Down",
        context: BindingContext::Selection,
        description: "Move through selection lists and menus",
    },
    KeyBinding {
        id: Some(KeyBindingId::ScrollPageUp),
        shortcut: "Page Up / Page Down",
        context: BindingContext::Chat,
        description: "Scroll chat, or move by a page in lists",
    },
    KeyBinding {
        id: Some(KeyBindingId::StepBack),
        shortcut: "Alt+Up / Alt+Down",
        context: BindingContext::Chat,
        description: "Step backward or forward through session history",
    },
    KeyBinding {
        id: Some(KeyBindingId::ApplyCompletion),
        shortcut: "Tab",
        context: BindingContext::Chat,
        description: "Apply the highlighted completion",
    },
    KeyBinding {
        id: Some(KeyBindingId::Submit),
        shortcut: "Enter",
        context: BindingContext::Global,
        description: "Confirm the current action, submit input, or open actions for the current context",
    },
    KeyBinding {
        id: Some(KeyBindingId::InsertNewline),
        shortcut: "Shift+Enter",
        context: BindingContext::Chat,
        description: "Insert a newline in chat input",
    },
    KeyBinding {
        id: Some(KeyBindingId::CycleShell),
        shortcut: "Ctrl+S",
        context: BindingContext::Shell,
        description: "Cycle between available shells",
    },
    KeyBinding {
        id: Some(KeyBindingId::ExitShellOnEmptyBackspace),
        shortcut: "Backspace",
        context: BindingContext::Shell,
        description: "Leave shell mode when shell input is empty",
    },
    KeyBinding {
        id: None,
        shortcut: "Type",
        context: BindingContext::Selection,
        description: "Filter list items",
    },
    KeyBinding {
        id: Some(KeyBindingId::EditProvider),
        shortcut: "Ctrl+E",
        context: BindingContext::ProviderPicker,
        description: "Edit the selected custom provider",
    },
    KeyBinding {
        id: Some(KeyBindingId::RemoveProvider),
        shortcut: "Ctrl+R",
        context: BindingContext::ProviderPicker,
        description: "Remove the selected custom provider",
    },
    KeyBinding {
        id: None,
        shortcut: "Scroll wheel",
        context: BindingContext::Mouse,
        description: "Scroll chat (3 lines per tick)",
    },
];

pub(crate) fn matches(id: KeyBindingId, key: KeyEvent) -> bool {
    match id {
        KeyBindingId::ShowHelp => key.code == KeyCode::F(1) && key.modifiers.is_empty(),
        KeyBindingId::Quit => {
            key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL)
        }
        KeyBindingId::QuitIfInputEmpty => {
            key.code == KeyCode::Char('d') && key.modifiers.contains(KeyModifiers::CONTROL)
        }
        KeyBindingId::ToggleInfo => {
            key.code == KeyCode::Char('i') && key.modifiers.contains(KeyModifiers::CONTROL)
        }
        KeyBindingId::ToggleInfoAlt => {
            key.code == KeyCode::Char('s') && key.modifiers.contains(KeyModifiers::ALT)
        }
        KeyBindingId::CopyLastAssistantResponse => {
            key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::ALT)
        }
        KeyBindingId::ToggleFullOutput => {
            key.code == KeyCode::Char('f') && key.modifiers.contains(KeyModifiers::CONTROL)
        }
        KeyBindingId::ResumeLatestSession => {
            key.code == KeyCode::Char('r') && key.modifiers.contains(KeyModifiers::CONTROL)
        }
        KeyBindingId::CycleShell => {
            key.code == KeyCode::Char('s') && key.modifiers.contains(KeyModifiers::CONTROL)
        }
        KeyBindingId::EditProvider => {
            key.code == KeyCode::Char('e') && key.modifiers.contains(KeyModifiers::CONTROL)
        }
        KeyBindingId::RemoveProvider => {
            key.code == KeyCode::Char('r') && key.modifiers.contains(KeyModifiers::CONTROL)
        }
        KeyBindingId::EnterShellMode => {
            key.code == KeyCode::Char('!')
                && (key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT)
        }
        KeyBindingId::StepBack => {
            (key.code == KeyCode::Up || key.code == KeyCode::Down)
                && key.modifiers.contains(KeyModifiers::ALT)
        }
        KeyBindingId::ScrollPageUp => key.code == KeyCode::PageUp || key.code == KeyCode::PageDown,
        KeyBindingId::Submit => key.code == KeyCode::Enter && key.modifiers.is_empty(),
        KeyBindingId::InsertNewline => {
            key.code == KeyCode::Enter && key.modifiers == KeyModifiers::SHIFT
        }
        KeyBindingId::ApplyCompletion => key.code == KeyCode::Tab,
        KeyBindingId::Cancel => key.code == KeyCode::Esc,
        KeyBindingId::ExitShellOnEmptyBackspace => key.code == KeyCode::Backspace,
        KeyBindingId::SelectionUp => {
            (key.code == KeyCode::Up || key.code == KeyCode::Down) && key.modifiers.is_empty()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{BindingContext, KEYBINDINGS, KeyBindingId};

    #[test]
    fn help_modal_binding_is_listed() {
        assert!(KEYBINDINGS.iter().any(|binding| {
            binding.id == Some(KeyBindingId::ShowHelp)
                && binding.shortcut == "F1"
                && binding.context == BindingContext::Global
        }));
    }

    #[test]
    fn keyboard_help_covers_multiple_contexts() {
        assert!(
            KEYBINDINGS
                .iter()
                .any(|binding| binding.context == BindingContext::Chat)
        );
        assert!(
            KEYBINDINGS
                .iter()
                .any(|binding| binding.context == BindingContext::Shell)
        );
        assert!(
            KEYBINDINGS
                .iter()
                .any(|binding| binding.context == BindingContext::Selection)
        );
    }
}
