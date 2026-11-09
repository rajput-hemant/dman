//! Toasts: short-lived notices stacked at the bottom right, above the footer.
//!
//! Every info / warning / error message set on the status ([`crate::app::Status`])
//! becomes a toast; progress messages ("Pausing…") do not. Info toasts last
//! 4 s, warnings 6 s, errors 8 s. A toast can carry an action (undo of a
//! remove-from-list) that its key runs while the toast is visible.

use std::time::Duration;

use dman_proto::JobId;
use dman_proto::method::Level;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};

use crate::app::State;
use crate::format::truncate;
use crate::layout::Hit;

/// How long a toast stays.
pub fn ttl(level: Level) -> Duration {
    match level {
        Level::Info => Duration::from_secs(4),
        Level::Warn => Duration::from_secs(6),
        Level::Error => Duration::from_secs(8),
    }
}

/// How long an undo stays possible.
pub const UNDO_TTL: Duration = Duration::from_secs(8);

/// Most toasts shown at once.
pub const MAX_SHOWN: usize = 3;

/// What a toast's key does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToastAction {
    /// Restore downloads removed from the list (files were kept).
    Undo(Vec<JobId>),
}

/// One notice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toast {
    /// Stable id (for clicks).
    pub id: u64,
    /// Severity.
    pub level: Level,
    /// Text.
    pub text: String,
    /// Optional action.
    pub action: Option<ToastAction>,
    /// Time shown so far.
    pub age: Duration,
    /// Time to live.
    pub ttl: Duration,
    /// How often the same text arrived in a row.
    pub count: u32,
}

/// The toast stack, oldest first.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Toasts {
    /// Live toasts, oldest first.
    pub items: Vec<Toast>,
    next: u64,
}

impl Toasts {
    /// Show `text`; the same text as the newest toast refreshes it instead.
    pub fn push(&mut self, level: Level, text: impl Into<String>) {
        self.push_full(level, text.into(), None, ttl(level));
    }

    /// Show a toast with an action and a custom lifetime.
    pub fn push_full(
        &mut self,
        level: Level,
        text: String,
        action: Option<ToastAction>,
        ttl: Duration,
    ) {
        if let Some(last) = self.items.last_mut()
            && last.text == text
            && last.level == level
            && last.action == action
        {
            last.age = Duration::ZERO;
            last.count += 1;
            return;
        }
        self.next += 1;
        self.items.push(Toast {
            id: self.next,
            level,
            text,
            action,
            age: Duration::ZERO,
            ttl,
            count: 1,
        });
        // Keep a short backlog only.
        let extra = self.items.len().saturating_sub(MAX_SHOWN + 2);
        self.items.drain(..extra);
    }

    /// A marker for [`Toasts::collapse_since`].
    pub fn mark(&self) -> u64 {
        self.next
    }

    /// Of the toasts created after `mark`, keep only the most important:
    /// one with an action, else the most severe, else the newest.
    pub fn collapse_since(&mut self, mark: u64) {
        let rank = |t: &Toast| {
            let sev = match t.level {
                Level::Info => 0,
                Level::Warn => 1,
                Level::Error => 2,
            };
            (u8::from(t.action.is_some()), sev, t.id)
        };
        let keep = self
            .items
            .iter()
            .filter(|t| t.id > mark)
            .max_by_key(|t| rank(t))
            .map(|t| t.id);
        if let Some(keep) = keep {
            self.items.retain(|t| t.id <= mark || t.id == keep);
        }
    }

    /// Age every toast by `dt`; `true` when one expired.
    pub fn tick(&mut self, dt: Duration) -> bool {
        let before = self.items.len();
        for t in &mut self.items {
            t.age += dt;
        }
        self.items.retain(|t| t.age < t.ttl);
        self.items.len() != before
    }

    /// Take the newest live undo, removing its toast.
    pub fn take_undo(&mut self) -> Option<Vec<JobId>> {
        let i = self
            .items
            .iter()
            .rposition(|t| matches!(t.action, Some(ToastAction::Undo(_))))?;
        self.items
            .remove(i)
            .action
            .map(|ToastAction::Undo(ids)| ids)
    }

    /// Whether an undo is offered right now.
    pub fn has_undo(&self) -> bool {
        self.items
            .iter()
            .any(|t| matches!(t.action, Some(ToastAction::Undo(_))))
    }

    /// Drop the toast `id`.
    pub fn dismiss(&mut self, id: u64) {
        self.items.retain(|t| t.id != id);
    }

    /// Drop every undo offer (e.g. after the files were deleted after all).
    pub fn drop_undo(&mut self) {
        self.items
            .retain(|t| !matches!(t.action, Some(ToastAction::Undo(_))));
    }
}

/// Draw the newest toasts at the bottom right of `area` (the body), newest
/// lowest; records click targets.
pub fn render(frame: &mut Frame<'_>, area: Rect, state: &State) {
    let t = &state.theme;
    let shown: Vec<&Toast> = state
        .status
        .toasts
        .items
        .iter()
        .rev()
        .take(MAX_SHOWN)
        .collect();
    if shown.is_empty() || area.height < 3 || area.width < 20 {
        return;
    }
    let max_w = usize::from(area.width.saturating_sub(2)).min(72);
    let undo_key = crate::widgets::footer::key_label_for(state, crate::action::Action::Undo);
    let mut y = area.y + area.height;
    for toast in shown {
        if y <= area.y {
            break;
        }
        y -= 1;
        let (icon, style) = match toast.level {
            Level::Info if toast.text.ends_with('…') => (t.icons.info, t.info),
            Level::Info => (t.icons.done, t.ok),
            Level::Warn => (t.icons.warn, t.warn),
            Level::Error => (t.icons.error, t.error),
        };
        let mut tail = String::new();
        if toast.count > 1 {
            tail.push_str(&format!(" ×{}", toast.count));
        }
        let action = match (&toast.action, &undo_key) {
            (Some(ToastAction::Undo(_)), Some(k)) => Some(format!(" {k} ")),
            _ => None,
        };
        let action_w = action.as_ref().map_or(0, |a| a.chars().count() + 6);
        let room = max_w.saturating_sub(5 + tail.chars().count() + action_w);
        let text = truncate(&toast.text, room);
        let mut spans = vec![
            Span::styled(t.icons.cursor, style.patch(t.surface_bg())),
            Span::styled(format!("{icon} "), style.patch(t.surface_bg())),
            Span::styled(text, t.surface),
            Span::styled(tail, t.dim.patch(t.surface_bg())),
        ];
        if let Some(a) = action {
            spans.push(Span::styled("  ", t.surface));
            spans.push(Span::styled(a, t.pill));
            spans.push(Span::styled(" undo", t.dim.patch(t.surface_bg())));
        }
        spans.push(Span::styled(" ", t.surface));
        let line = Line::from(spans);
        let w = u16::try_from(line.width())
            .unwrap_or(area.width)
            .min(area.width);
        let rect = Rect {
            x: area.x + area.width - w,
            y,
            width: w,
            height: 1,
        };
        frame.render_widget(Clear, rect);
        frame.render_widget(Paragraph::new(line), rect);
        state.hit(rect, Hit::Toast(toast.id));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expiry_dedupe_and_undo() {
        let mut t = Toasts::default();
        t.push(Level::Info, "a");
        t.push(Level::Info, "a");
        assert_eq!(t.items.len(), 1);
        assert_eq!(t.items[0].count, 2);
        t.push(Level::Error, "b");
        t.push_full(
            Level::Info,
            "removed".into(),
            Some(ToastAction::Undo(vec![JobId(1)])),
            UNDO_TTL,
        );
        assert!(t.has_undo());
        assert!(t.tick(Duration::from_secs(5)));
        assert_eq!(t.items.len(), 2, "the info toast expired");
        assert_eq!(t.take_undo(), Some(vec![JobId(1)]));
        assert!(!t.has_undo());
        assert!(t.tick(Duration::from_secs(4)));
        assert!(t.items.is_empty());
    }
}
