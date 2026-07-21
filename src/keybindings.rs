use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum KeyBindingId {
    ShowHelp,
    Abort,
    EndInput,
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
        description: "Cancel or close the current context; clear input when idle",
    },
    KeyBinding {
        id: Some(KeyBindingId::Abort),
        shortcut: "Ctrl+C",
        context: BindingContext::Global,
        description: "Abort agent loop (1: stop after turn, 2: abort, 3: force kill)",
    },
    KeyBinding {
        id: Some(KeyBindingId::EndInput),
        shortcut: "Ctrl+D",
        context: BindingContext::Global,
        description: "End input / quit (press twice while agent is running)",
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
        KeyBindingId::Abort => {
            key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL)
        }
        KeyBindingId::EndInput => {
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
    use super::{BindingContext, KEYBINDINGS, KeyBindingId, matches};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    // ── matches() ─────────────────────────────────────────────────────────

    #[test]
    fn matches_each_binding_id_with_correct_key() {
        let cases: &[(KeyBindingId, KeyEvent)] = &[
            (
                KeyBindingId::ShowHelp,
                KeyEvent::new(KeyCode::F(1), KeyModifiers::empty()),
            ),
            (
                KeyBindingId::Abort,
                KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
            ),
            (
                KeyBindingId::EndInput,
                KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL),
            ),
            (
                KeyBindingId::ToggleInfo,
                KeyEvent::new(KeyCode::Char('i'), KeyModifiers::CONTROL),
            ),
            (
                KeyBindingId::ToggleInfoAlt,
                KeyEvent::new(KeyCode::Char('s'), KeyModifiers::ALT),
            ),
            (
                KeyBindingId::CopyLastAssistantResponse,
                KeyEvent::new(KeyCode::Char('c'), KeyModifiers::ALT),
            ),
            (
                KeyBindingId::ToggleFullOutput,
                KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL),
            ),
            (
                KeyBindingId::ResumeLatestSession,
                KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL),
            ),
            (
                KeyBindingId::CycleShell,
                KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL),
            ),
            (
                KeyBindingId::EditProvider,
                KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL),
            ),
            (
                KeyBindingId::RemoveProvider,
                KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL),
            ),
            (
                KeyBindingId::EnterShellMode,
                KeyEvent::new(KeyCode::Char('!'), KeyModifiers::empty()),
            ),
            (
                KeyBindingId::StepBack,
                KeyEvent::new(KeyCode::Up, KeyModifiers::ALT),
            ),
            (
                KeyBindingId::ScrollPageUp,
                KeyEvent::new(KeyCode::PageUp, KeyModifiers::empty()),
            ),
            (
                KeyBindingId::Submit,
                KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
            ),
            (
                KeyBindingId::InsertNewline,
                KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT),
            ),
            (
                KeyBindingId::ApplyCompletion,
                KeyEvent::new(KeyCode::Tab, KeyModifiers::empty()),
            ),
            (
                KeyBindingId::Cancel,
                KeyEvent::new(KeyCode::Esc, KeyModifiers::empty()),
            ),
            (
                KeyBindingId::ExitShellOnEmptyBackspace,
                KeyEvent::new(KeyCode::Backspace, KeyModifiers::empty()),
            ),
            (
                KeyBindingId::SelectionUp,
                KeyEvent::new(KeyCode::Up, KeyModifiers::empty()),
            ),
        ];

        for (id, key) in cases {
            assert!(
                matches(*id, *key),
                "expected matches({id:?}, {key:?}) == true"
            );
        }
    }

    #[test]
    fn matches_rejects_wrong_key_for_binding() {
        // Ctrl+C should not match ToggleInfo
        assert!(!matches(
            KeyBindingId::ToggleInfo,
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)
        ));
        // Alt+S should not match CycleShell (which is Ctrl+S)
        assert!(!matches(
            KeyBindingId::CycleShell,
            KeyEvent::new(KeyCode::Char('s'), KeyModifiers::ALT)
        ));
        // Enter+Shift should not match Submit (which requires no modifiers)
        assert!(!matches(
            KeyBindingId::Submit,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT)
        ));
    }

    #[test]
    fn matches_enter_shell_mode_with_shift() {
        // ! with Shift should also match EnterShellMode
        assert!(matches(
            KeyBindingId::EnterShellMode,
            KeyEvent::new(KeyCode::Char('!'), KeyModifiers::SHIFT)
        ));
    }

    #[test]
    fn matches_step_back_with_alt_down() {
        assert!(matches(
            KeyBindingId::StepBack,
            KeyEvent::new(KeyCode::Down, KeyModifiers::ALT)
        ));
    }

    #[test]
    fn matches_scroll_page_up_with_page_down() {
        assert!(matches(
            KeyBindingId::ScrollPageUp,
            KeyEvent::new(KeyCode::PageDown, KeyModifiers::empty())
        ));
    }

    #[test]
    fn matches_selection_up_with_down() {
        assert!(matches(
            KeyBindingId::SelectionUp,
            KeyEvent::new(KeyCode::Down, KeyModifiers::empty())
        ));
    }

    // ── table ─────────────────────────────────────────────────────────────

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

    #[test]
    fn every_keybinding_id_in_table_has_a_matches_arm() {
        // Every binding with an id in the table should match some key event.
        for binding in KEYBINDINGS {
            if let Some(id) = binding.id {
                // Exercise matches() — not asserting the result, just that
                // the match arm exists and doesn't panic.
                let _ = matches(id, KeyEvent::new(KeyCode::F(1), KeyModifiers::empty()));
            }
        }
    }
}
