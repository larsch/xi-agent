use ratatui_textarea::TextArea;

use crate::shell::{self, ShellKind};

/// State owned by the shell input mode.
pub struct ShellState {
    pub(crate) textarea: TextArea<'static>,
    pub(crate) selected: ShellKind,
    pub(crate) available: Vec<ShellKind>,
}

impl ShellState {
    pub fn new() -> Self {
        let available = shell::discover_available_shells();
        let selected = available.first().copied().unwrap_or(ShellKind::Bash);
        Self {
            textarea: Self::make_textarea(),
            selected,
            available,
        }
    }

    pub(crate) fn make_textarea() -> TextArea<'static> {
        TextArea::default()
    }

    pub fn reset_textarea(&mut self) {
        self.textarea = Self::make_textarea();
    }

    pub fn input_is_empty(&self) -> bool {
        self.textarea
            .lines()
            .iter()
            .all(|line| line.trim().is_empty())
    }

    pub fn cycle(&mut self) {
        if self.available.len() <= 1 {
            return;
        }
        let idx = self
            .available
            .iter()
            .position(|s| *s == self.selected)
            .unwrap_or(0);
        self.selected = self.available[(idx + 1) % self.available.len()];
    }
}

impl Default for ShellState {
    fn default() -> Self {
        Self::new()
    }
}
