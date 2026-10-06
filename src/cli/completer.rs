//! Slash-command menu for the interactive CLI.
//!
//! Behavior:
//! - Typing `/` immediately shows a filtered menu below the input line.
//! - Typing more narrows the menu by command prefix (`/n` → `/new`, …).
//! - Up/Down cycles the highlighted match in the menu.
//! - Tab accepts the highlighted command into the input line.

use std::sync::{Arc, Mutex};

use rustyline::completion::{Completer, Pair};
use rustyline::highlight::Highlighter;
use rustyline::hint::{Hint, Hinter};
use rustyline::validate::Validator;
use rustyline::{
    Cmd, ConditionalEventHandler, Context, Event, EventContext, EventHandler, Helper, Movement,
};

use super::registry;
use super::style;

const MAX_VISIBLE_COMMANDS: usize = 8;

#[derive(Default, Debug)]
struct SlashMenuState {
    partial: String,
    selected: usize,
}

#[derive(Clone)]
pub struct SlashMenuHint(String);

impl Hint for SlashMenuHint {
    fn display(&self) -> &str {
        &self.0
    }

    fn completion(&self) -> Option<&str> {
        None
    }
}

#[derive(Clone, Default)]
pub struct PhoenixHelper {
    state: Arc<Mutex<SlashMenuState>>,
}

impl PhoenixHelper {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn bind_editor(
        &self,
        editor: &mut rustyline::Editor<PhoenixHelper, rustyline::history::DefaultHistory>,
    ) {
        editor.bind_sequence(
            Event::from(rustyline::KeyEvent(
                rustyline::KeyCode::Up,
                rustyline::Modifiers::NONE,
            )),
            EventHandler::Conditional(Box::new(SlashMenuNav {
                state: Arc::clone(&self.state),
                delta: -1,
            })),
        );
        editor.bind_sequence(
            Event::from(rustyline::KeyEvent(
                rustyline::KeyCode::Down,
                rustyline::Modifiers::NONE,
            )),
            EventHandler::Conditional(Box::new(SlashMenuNav {
                state: Arc::clone(&self.state),
                delta: 1,
            })),
        );
        editor.bind_sequence(
            Event::from(rustyline::KeyEvent(
                rustyline::KeyCode::Tab,
                rustyline::Modifiers::NONE,
            )),
            EventHandler::Conditional(Box::new(SlashMenuAccept {
                state: Arc::clone(&self.state),
            })),
        );
    }

    fn sync_selection(&self, partial: &str, len: usize) -> usize {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if state.partial != partial {
            state.partial = partial.to_string();
            state.selected = 0;
        }
        if len == 0 {
            state.selected = 0;
        } else if state.selected >= len {
            state.selected = len - 1;
        }
        state.selected
    }

    fn menu_hint(&self, partial: &str) -> Option<String> {
        let matches = registry::matches(partial);
        if matches.is_empty() {
            return None;
        }
        let selected = self.sync_selection(partial, matches.len());
        let start = if matches.len() <= MAX_VISIBLE_COMMANDS {
            0
        } else if selected < MAX_VISIBLE_COMMANDS / 2 {
            0
        } else if selected + MAX_VISIBLE_COMMANDS / 2 >= matches.len() {
            matches.len() - MAX_VISIBLE_COMMANDS
        } else {
            selected - MAX_VISIBLE_COMMANDS / 2
        };
        let end = (start + MAX_VISIBLE_COMMANDS).min(matches.len());

        let mut out = String::new();
        out.push('\n');
        for (idx, cmd) in matches[start..end].iter().enumerate() {
            let absolute_idx = start + idx;
            let marker = if absolute_idx == selected { '›' } else { ' ' };
            let args = if cmd.args.is_empty() {
                String::new()
            } else {
                format!(" {}", cmd.args)
            };
            let command = format!("/{}{}", cmd.name, args);
            let row = if absolute_idx == selected {
                format!(
                    "  {marker} {}  {}",
                    style::bold(&command),
                    style::dim(cmd.desc)
                )
            } else {
                format!(
                    "  {marker} {}  {}",
                    style::dim(&command),
                    style::dim(cmd.desc)
                )
            };
            out.push_str(&row);
            out.push('\n');
        }
        if matches.len() > end {
            out.push_str(&format!(
                "  {} {} more\n",
                style::dim("…"),
                matches.len() - end
            ));
        }
        Some(out)
    }
}

fn command_replacement(cmd: &registry::SlashCommand) -> String {
    if cmd.args.is_empty() {
        format!("/{}", cmd.name)
    } else {
        format!("/{} ", cmd.name)
    }
}

/// Only the leading command word (before any space) participates in slash mode.
fn command_word(line: &str) -> Option<&str> {
    let rest = line.strip_prefix('/')?;
    if rest.contains(char::is_whitespace) {
        return None;
    }
    Some(rest)
}

impl Completer for PhoenixHelper {
    type Candidate = Pair;

    fn complete(
        &self,
        line: &str,
        _pos: usize,
        _ctx: &Context<'_>,
    ) -> rustyline::Result<(usize, Vec<Pair>)> {
        // Keep rustyline from replacing the input inline; the menu below handles UX.
        let _ = command_word(line);
        Ok((0, Vec::new()))
    }
}

impl Hinter for PhoenixHelper {
    type Hint = SlashMenuHint;

    fn hint(&self, line: &str, pos: usize, _ctx: &Context<'_>) -> Option<Self::Hint> {
        if pos != line.len() {
            return None;
        }
        let partial = command_word(line)?;
        self.menu_hint(partial).map(SlashMenuHint)
    }
}

#[derive(Clone)]
struct SlashMenuNav {
    state: Arc<Mutex<SlashMenuState>>,
    delta: isize,
}

impl ConditionalEventHandler for SlashMenuNav {
    fn handle(
        &self,
        _evt: &Event,
        _n: rustyline::RepeatCount,
        _positive: bool,
        ctx: &EventContext,
    ) -> Option<Cmd> {
        let partial = command_word(ctx.line())?;
        let matches = registry::matches(partial);
        if matches.is_empty() {
            return None;
        }
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if state.partial != partial {
            state.partial = partial.to_string();
            state.selected = 0;
        } else if self.delta < 0 {
            state.selected = if state.selected == 0 {
                matches.len() - 1
            } else {
                state.selected - 1
            };
        } else {
            state.selected = (state.selected + 1) % matches.len();
        }
        Some(Cmd::Repaint)
    }
}

#[derive(Clone)]
struct SlashMenuAccept {
    state: Arc<Mutex<SlashMenuState>>,
}

impl ConditionalEventHandler for SlashMenuAccept {
    fn handle(
        &self,
        _evt: &Event,
        _n: rustyline::RepeatCount,
        _positive: bool,
        ctx: &EventContext,
    ) -> Option<Cmd> {
        let partial = command_word(ctx.line())?;
        let matches = registry::matches(partial);
        if matches.is_empty() {
            return None;
        }
        let selected = {
            let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
            if state.partial != partial {
                state.partial = partial.to_string();
                state.selected = 0;
            }
            state.selected.min(matches.len() - 1)
        };
        Some(Cmd::Replace(
            Movement::WholeLine,
            Some(command_replacement(matches[selected])),
        ))
    }
}

impl Highlighter for PhoenixHelper {}
impl Validator for PhoenixHelper {}
impl Helper for PhoenixHelper {}

#[cfg(test)]
mod tests {
    use super::*;
    use rustyline::history::DefaultHistory;

    #[test]
    fn command_word_allows_empty_after_slash() {
        assert_eq!(command_word("/"), Some(""));
        assert_eq!(command_word("/n"), Some("n"));
        assert_eq!(command_word("/new foo"), None);
        assert_eq!(command_word("plain"), None);
    }

    #[test]
    fn hint_shows_filtered_menu() {
        let helper = PhoenixHelper::new();
        let history = DefaultHistory::new();
        let ctx = Context::new(&history);
        let hint = helper.hint("/n", 2, &ctx).unwrap();
        assert!(hint.display().contains("/new"));
        assert!(!hint.display().contains("/help"));
        assert!(hint.completion().is_none());
    }

    #[test]
    fn completer_does_not_inline_replace() {
        let helper = PhoenixHelper::new();
        let history = DefaultHistory::new();
        let ctx = Context::new(&history);
        let (_, pairs) = helper.complete("/", 1, &ctx).unwrap();
        assert!(pairs.is_empty());
    }
}
