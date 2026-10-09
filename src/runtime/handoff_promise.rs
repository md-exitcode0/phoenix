//! Catch a reply that promises to contact a coworker ("I'll tell Leon…",
//! "sending this to Leon") when the turn never actually reached that
//! coworker with `message_agent` or `talk`. Nothing else in the turn loop
//! connects the words of a reply to a delivery, so a model could end its turn
//! on the promise alone and the user would wait for a handoff that never
//! happened. Pure text logic lives here so it is unit-testable.

use std::collections::HashSet;

/// One coworker the current agent could contact: the routing key (role id,
/// lowercase) and every name the user or the model might use for them.
#[derive(Debug, Clone)]
pub struct Coworker {
    pub key: String,
    pub display_name: String,
    pub names: Vec<String>,
}

impl Coworker {
    pub fn new(key: &str, display_name: &str, extra: &[&str]) -> Self {
        let mut names = Vec::new();
        let mut push = |value: &str| {
            let value = value.trim().to_ascii_lowercase();
            if value.chars().count() >= 3 && !names.contains(&value) {
                names.push(value);
            }
        };
        // Only the names a person would write ("Leon", "Leon Lin"). Role ids
        // ("researcher", "coder") read as ordinary nouns in prose.
        push(display_name);
        if let Some(first) = display_name.split_whitespace().next() {
            push(first);
        }
        for value in extra {
            push(value);
        }
        Self {
            key: key.trim().to_ascii_lowercase(),
            display_name: display_name.trim().to_string(),
            names,
        }
    }
}

/// The active directory, minus the speaking agent itself.
pub fn coworkers_from_directory(self_label: &str) -> Vec<Coworker> {
    let me = crate::runtime::postbox::base_agent(self_label).to_ascii_lowercase();
    let Ok(company) = crate::runtime::company::global() else {
        return Vec::new();
    };
    let Ok(snapshot) = company.directory_snapshot() else {
        return Vec::new();
    };
    snapshot
        .agents
        .into_iter()
        .filter(|record| {
            record.profile.lifecycle == crate::runtime::company_directory::LifecycleState::Active
        })
        .filter(|record| {
            let id = record.profile.agent_id.to_ascii_lowercase();
            let role = record.profile.internal_role.to_ascii_lowercase();
            id != me && role != me && !(me == "orchestrator" && id == "phoenix")
        })
        .map(|record| {
            Coworker::new(&record.profile.internal_role, &record.profile.display_name, &[])
        })
        .collect()
}

const PROMISE_LEADS: &[&str] = &[
    "i'll", "i’ll", "i will", "i'm going to", "i’m going to", "i am going to", "let me",
    "i'm about to", "i’m about to", "next i'll", "next i’ll",
];
const CONTACT_VERBS: &[&str] = &[
    "ask", "tell", "have", "get", "message", "ping", "send", "hand", "pass", "loop in", "loop", "check with",
    "brief", "remind", "nudge", "notify", "forward", "reach out to", "contact", "follow up with",
];
const PROGRESSIVE: &[&str] = &[
    "sending", "asking", "messaging", "telling", "pinging", "handing", "passing", "forwarding",
];

fn normalize(text: &str) -> String {
    text.to_lowercase().replace(['\n', '\r', '\t'], " ")
}

/// Whole-word find of `needle` in `hay` starting at or after `from`.
fn word_at(hay: &str, needle: &str, from: usize) -> Option<usize> {
    let mut start = from;
    while start <= hay.len() {
        let found = hay.get(start..)?.find(needle)? + start;
        let before_ok = found == 0
            || !hay[..found].chars().next_back().is_some_and(|ch| ch.is_alphanumeric());
        let end = found + needle.len();
        let after_ok = end >= hay.len()
            || !hay[end..].chars().next().is_some_and(|ch| ch.is_alphanumeric());
        if before_ok && after_ok {
            return Some(found);
        }
        start = found + needle.len().max(1);
    }
    None
}

/// The clause around an index: from the previous sentence break to the next.
fn clause(hay: &str, at: usize) -> &str {
    let start = hay[..at]
        .rfind(['.', '!', '?', ';', '\n'])
        .map(|index| index + 1)
        .unwrap_or(0);
    let end = hay[at..]
        .find(['.', '!', '?', ';', '\n'])
        .map(|index| at + index)
        .unwrap_or(hay.len());
    &hay[start..end]
}

fn promises_contact(sentence: &str, name: &str) -> bool {
    let Some(name_at) = word_at(sentence, name, 0) else {
        return false;
    };
    let before = &sentence[..name_at];
    // "sending this to Leon", "asking Leon", "messaging Leon now"
    if PROGRESSIVE.iter().any(|verb| {
        word_at(before, verb, 0)
            .is_some_and(|at| before[at + verb.len()..].split_whitespace().count() <= 3)
    }) && !before.contains("was ")
        && !before.contains("already")
    {
        return true;
    }
    // "I'll ask Leon", "I will have Leon…", "let me check with Leon",
    // "I'll send this to Leon": the contact verb sits at most three words
    // before the name, so "I'll have the site ready before Leon…" is not one.
    PROMISE_LEADS.iter().any(|lead| {
        word_at(before, lead, 0).is_some_and(|lead_at| {
            let tail = &before[lead_at + lead.len()..];
            tail.split_whitespace().count() <= 8
                && CONTACT_VERBS.iter().any(|verb| {
                    let mut from = 0;
                    while let Some(verb_at) = word_at(tail, verb, from) {
                        if tail[verb_at + verb.len()..].split_whitespace().count() <= 3 {
                            return true;
                        }
                        from = verb_at + verb.len();
                    }
                    false
                })
        })
    })
}

/// First coworker the texts promise to contact who was not contacted.
pub fn unfulfilled_promise<'a>(
    texts: &[&str],
    coworkers: &'a [Coworker],
    contacted: &HashSet<String>,
) -> Option<&'a Coworker> {
    let texts: Vec<String> = texts.iter().map(|text| normalize(text)).collect();
    coworkers.iter().find(|coworker| {
        if contacted.contains(&coworker.key)
            || coworker.names.iter().any(|name| contacted.contains(name))
        {
            return false;
        }
        texts.iter().any(|text| {
            coworker.names.iter().any(|name| {
                let mut from = 0;
                while let Some(at) = word_at(text, name, from) {
                    if promises_contact(clause(text, at), name) {
                        return true;
                    }
                    from = at + name.len();
                }
                false
            })
        })
    })
}

pub fn corrective_feedback(coworker: &Coworker) -> String {
    let name = if coworker.display_name.is_empty() { coworker.key.as_str() } else { coworker.display_name.as_str() };
    format!(
        "Your reply says you will contact {name}, but nothing was sent to {name} in this turn. Words do not deliver a message. Either call message_agent (context or a notice) or talk (work you need {name} to do) to `{key}` now and then finish, or rewrite your reply so it does not claim you contacted or will contact {name}. Do not promise a handoff you have not made.",
        key = coworker.key
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn team() -> Vec<Coworker> {
        vec![
            Coworker::new("designer", "Leon Lin", &[]),
            Coworker::new("researcher", "Theo", &[]),
        ]
    }

    #[test]
    fn detects_promises_without_delivery() {
        let team = team();
        let none = HashSet::new();
        for text in [
            "Got it. I'll tell Leon you've fixed the bugs and have him pick up the website.",
            "I’ll ask Leon to turn this into a visual guide.",
            "Sending this to Leon now.",
            "Let me check with Theo first.",
            "I will have Leon Lin rebuild the page.",
        ] {
            assert!(unfulfilled_promise(&[text], &team, &none).is_some(), "{text}");
        }
    }

    #[test]
    fn ignores_past_tense_other_subjects_and_delivered_contacts() {
        let team = team();
        let none = HashSet::new();
        for text in [
            "I asked Leon earlier and he replied.",
            "Leon will finish the layout tonight.",
            "Leon's report says the build is done.",
            "I'll open the finished guide in my browser once it's ready.",
            "I already sent this to Leon.",
            "I'll have the site ready before Leon starts.",
            "I'll let you know when Leon is done.",
        ] {
            assert!(unfulfilled_promise(&[text], &team, &none).is_none(), "{text}");
        }
        let mut contacted = HashSet::new();
        contacted.insert("designer".to_string());
        assert!(unfulfilled_promise(&["I'll tell Leon now."], &team, &contacted).is_none());
        assert!(corrective_feedback(&team[0]).contains("message_agent"));
    }
}
