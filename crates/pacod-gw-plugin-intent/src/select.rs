//! What part of a conversation is classified (`docs/plugin-concept.md`, section 5.3).

use pacod_gw_plugin::{Conversation, Role};

use crate::settings::{Scope, Settings};

/// About four characters make a token; the budget is counted in characters.
const CHARS_PER_TOKEN: usize = 4;

/// One piece of the text to classify, as an engine is given it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Piece {
    /// `system`, `user`, `assistant` or `tool`.
    pub from: &'static str,
    pub text: String,
}

/// The text to classify, oldest first.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Selection {
    pub pieces: Vec<Piece>,
    /// Text was left out to stay within the budget.
    pub truncated: bool,
}

impl Selection {
    pub fn is_empty(&self) -> bool {
        self.pieces.is_empty()
    }

    /// The pieces as one transcript, a line per piece: `user: …`.
    pub fn transcript(&self) -> String {
        self.pieces.iter().map(|p| format!("{}: {}", p.from, p.text)).collect::<Vec<_>>().join("\n")
    }
}

/// The text of `c` the settings ask to classify, within `max_input_tokens`. The newest text is
/// what the user asks now, so the oldest goes first: the newest piece keeps its end, marked
/// `[…]`, when it alone is too long; older pieces are taken whole or not at all.
pub fn select(c: &Conversation, s: &Settings) -> Selection {
    let turns: Vec<(&'static str, &str)> = c
        .turns
        .iter()
        .filter(|t| t.role != Role::Tool || s.include_tool_results)
        .map(|t| (t.role.name(), t.text.as_str()))
        .collect();
    let chosen: Vec<(&'static str, &str)> = match s.scope {
        Scope::LastUser => c.last_user().map(|t| vec![("user", t.text.as_str())]).unwrap_or_default(),
        Scope::LastTurns => turns[turns.len().saturating_sub(usize::from(s.turns))..].to_vec(),
        Scope::Conversation => turns,
    };
    let system: Vec<(&'static str, &str)> =
        if s.include_system { c.system.iter().map(|t| ("system", t.as_str())).collect() } else { Vec::new() };

    let mut budget = (s.max_input_tokens as usize).saturating_mul(CHARS_PER_TOKEN);
    let mut out = Selection::default();
    // From the newest piece back; the system prompt comes before every turn, so it goes first
    // when there is no room.
    for (from, text) in chosen.into_iter().rev().chain(system.into_iter().rev()) {
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        if budget == 0 {
            out.truncated = true;
            break;
        }
        let n = text.chars().count();
        if n <= budget {
            budget -= n;
            out.pieces.push(Piece { from, text: text.to_owned() });
        } else if out.pieces.is_empty() {
            let tail: String = text.chars().skip(n - budget).collect();
            out.pieces.push(Piece { from, text: format!("[…]{tail}") });
            out.truncated = true;
            budget = 0;
        } else {
            out.truncated = true;
            break;
        }
    }
    out.pieces.reverse();
    out
}

#[cfg(test)]
mod tests {
    use pacod_gw_plugin::Turn;

    use super::*;

    fn conv() -> Conversation {
        let t = |role, text: &str| Turn { role, text: text.to_owned() };
        Conversation {
            system: vec!["You are a support bot.".into()],
            turns: vec![
                t(Role::User, "first question"),
                t(Role::Assistant, "an answer"),
                t(Role::Tool, "tool output"),
                t(Role::User, "the newest question"),
            ],
            ..Default::default()
        }
    }

    fn texts(s: &Selection) -> Vec<(&str, &str)> {
        s.pieces.iter().map(|p| (p.from, p.text.as_str())).collect()
    }

    #[test]
    fn the_last_user_turn_by_default() {
        let s = select(&conv(), &Settings::default());
        assert_eq!(texts(&s), vec![("user", "the newest question")]);
        assert_eq!(s.transcript(), "user: the newest question");
    }

    #[test]
    fn turns_conversation_system_and_tools() {
        let mut st = Settings { scope: Scope::LastTurns, turns: 2, ..Default::default() };
        assert_eq!(texts(&select(&conv(), &st)), vec![("assistant", "an answer"), ("user", "the newest question")]);
        st.include_tool_results = true;
        assert_eq!(texts(&select(&conv(), &st)), vec![("tool", "tool output"), ("user", "the newest question")]);
        let st = Settings { scope: Scope::Conversation, include_system: true, ..Default::default() };
        let s = select(&conv(), &st);
        assert_eq!(s.pieces.first().map(|p| p.from), Some("system"));
        assert_eq!(s.pieces.len(), 4, "the system prompt and three turns, no tool output");
    }

    #[test]
    fn the_oldest_text_goes_first() {
        let st =
            Settings { scope: Scope::Conversation, include_system: true, max_input_tokens: 5, ..Default::default() };
        let s = select(&conv(), &st);
        assert!(s.truncated);
        assert_eq!(texts(&s), vec![("user", "the newest question")], "20 characters: older turns do not fit whole");
        let st = Settings { max_input_tokens: 3, ..Default::default() };
        assert_eq!(
            texts(&select(&conv(), &st)),
            vec![("user", "[…]est question")],
            "the newest keeps its last 12 characters"
        );
        assert!(select(&Conversation::default(), &Settings::default()).is_empty());
    }
}
