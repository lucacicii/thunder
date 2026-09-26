use crossterm::event::{KeyCode, KeyEvent};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PickerKind {
    ResumeSession,
    SelectModel,
    SelectMode,
    SelectSkill,
    SelectMcp,
    SlashCommand,
}

impl PickerKind {
    pub fn default_title(&self) -> &'static str {
        match self {
            Self::ResumeSession => "📁 Resume Conversation Session",
            Self::SelectModel => "🧠 Select Active LLM Model",
            Self::SelectMode => "⚡ Select Multi-Agent Execution Mode",
            Self::SelectSkill => "📖 Browse & Load Agent Skill",
            Self::SelectMcp => "🔌 Connected MCP Servers & Tools",
            Self::SlashCommand => "⚡ Slash Commands",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PickerItem {
    pub id: String,
    pub title: String,
    pub description: String,
    pub badge: Option<String>,
}

impl PickerItem {
    pub fn new(
        id: impl Into<String>,
        title: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            description: description.into(),
            badge: None,
        }
    }

    pub fn with_badge(mut self, badge: impl Into<String>) -> Self {
        self.badge = Some(badge.into());
        self
    }
}

#[derive(Debug, Clone)]
pub struct PickerState {
    pub is_open: bool,
    pub kind: PickerKind,
    pub title: String,
    pub items: Vec<PickerItem>,
    pub selected_index: usize,
    pub filter_text: String,
}

impl Default for PickerState {
    fn default() -> Self {
        Self {
            is_open: false,
            kind: PickerKind::SlashCommand,
            title: String::new(),
            items: Vec::new(),
            selected_index: 0,
            filter_text: String::new(),
        }
    }
}

impl PickerState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Open picker with given kind, title, and item list.
    pub fn open(&mut self, kind: PickerKind, title: Option<String>, items: Vec<PickerItem>) {
        self.is_open = true;
        self.title = title.unwrap_or_else(|| kind.default_title().to_string());
        self.kind = kind;
        self.items = items;
        self.selected_index = 0;
        self.filter_text.clear();
    }

    /// Close and dismiss picker.
    pub fn close(&mut self) {
        self.is_open = false;
        self.items.clear();
        self.selected_index = 0;
        self.filter_text.clear();
    }

    /// Get items matching the current filter string.
    pub fn filtered_items(&self) -> Vec<&PickerItem> {
        if self.filter_text.is_empty() {
            return self.items.iter().collect();
        }

        let q_lower = self.filter_text.to_lowercase();
        self.items
            .iter()
            .filter(|item| {
                item.title.to_lowercase().contains(&q_lower)
                    || item.description.to_lowercase().contains(&q_lower)
                    || item.id.to_lowercase().contains(&q_lower)
                    || item
                        .badge
                        .as_ref()
                        .map(|b| b.to_lowercase().contains(&q_lower))
                        .unwrap_or(false)
            })
            .collect()
    }

    /// Get currently selected item (from filtered items).
    pub fn selected_item(&self) -> Option<PickerItem> {
        let filtered = self.filtered_items();
        if filtered.is_empty() {
            return None;
        }
        let idx = self.selected_index.min(filtered.len().saturating_sub(1));
        Some((*filtered[idx]).clone())
    }

    pub fn move_up(&mut self) {
        let count = self.filtered_items().len();
        if count > 0 {
            if self.selected_index == 0 {
                self.selected_index = count - 1;
            } else {
                self.selected_index -= 1;
            }
        }
    }

    pub fn move_down(&mut self) {
        let count = self.filtered_items().len();
        if count > 0 {
            self.selected_index = (self.selected_index + 1) % count;
        }
    }

    pub fn page_up(&mut self, step: usize) {
        self.selected_index = self.selected_index.saturating_sub(step);
    }

    pub fn page_down(&mut self, step: usize) {
        let count = self.filtered_items().len();
        if count > 0 {
            self.selected_index = (self.selected_index + step).min(count - 1);
        }
    }

    /// Handle key input when picker modal is active.
    /// Returns `Some(selected_item)` if user pressed Enter, `None` if navigating/filtering or Esc.
    pub fn handle_key(&mut self, key: KeyEvent) -> PickerResult {
        match key.code {
            KeyCode::Esc => {
                self.close();
                PickerResult::Cancelled
            }
            KeyCode::Up | KeyCode::Char('k')
                if key.modifiers.is_empty() && self.filter_text.is_empty() =>
            {
                self.move_up();
                PickerResult::Navigating
            }
            KeyCode::Down | KeyCode::Char('j')
                if key.modifiers.is_empty() && self.filter_text.is_empty() =>
            {
                self.move_down();
                PickerResult::Navigating
            }
            KeyCode::Up => {
                self.move_up();
                PickerResult::Navigating
            }
            KeyCode::Down => {
                self.move_down();
                PickerResult::Navigating
            }
            KeyCode::PageUp => {
                self.page_up(5);
                PickerResult::Navigating
            }
            KeyCode::PageDown => {
                self.page_down(5);
                PickerResult::Navigating
            }
            KeyCode::Home => {
                self.selected_index = 0;
                PickerResult::Navigating
            }
            KeyCode::End => {
                let count = self.filtered_items().len();
                if count > 0 {
                    self.selected_index = count - 1;
                }
                PickerResult::Navigating
            }
            KeyCode::Enter => {
                let item = self.selected_item();
                let kind = self.kind.clone();
                self.close();
                if let Some(selected) = item {
                    PickerResult::Selected(kind, selected)
                } else {
                    PickerResult::Cancelled
                }
            }
            KeyCode::Backspace => {
                self.filter_text.pop();
                self.selected_index = 0;
                PickerResult::Filtering
            }
            KeyCode::Char(c) => {
                self.filter_text.push(c);
                self.selected_index = 0;
                PickerResult::Filtering
            }
            _ => PickerResult::Ignored,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickerResult {
    Navigating,
    Filtering,
    Selected(PickerKind, PickerItem),
    Cancelled,
    Ignored,
}
