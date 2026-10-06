//! Append-only request history within one turn.
//!
//! The ChatGPT Codex backend reuses its prompt cache only when the previous
//! request is an exact prefix of the next one. Measured 2026-09-22 with a
//! 10k-token base over five rounds: replacing a trailing note each round kept
//! the cached prefix pinned at the first request (0, 0, 10240, 10240, 10240);
//! appending instead let it follow the conversation (0, 10240, 14464, 18688,
//! 22912). So everything a round adds — guidance notes, the todo map, image
//! observations — is committed into the turn's history once and never
//! rewritten, the way unreal-agent keeps a committed prefix.

use crate::providers::{ChatMessage, CompletionRequest};

/// Committed image messages that keep their pixels. Older ones are reduced to
/// their text labels in one batch once the count exceeds twice this, so the
/// turn pays one deliberate cache break per few new observations instead of
/// either re-sending every image forever or breaking the cache every round.
const PIXEL_MESSAGES_KEPT: usize = 3;

/// Move the volatile messages the caller appended after `[system, user]`
/// (plus `trailing`, e.g. image observations) into `history` — skipping any
/// that were already committed unchanged — then send `[system, user] + history`.
pub fn commit_round(request: &mut CompletionRequest, history: &mut Vec<ChatMessage>, trailing: Vec<ChatMessage>) {
    let lead = request.messages.len().min(2);
    let volatile: Vec<ChatMessage> = request.messages.drain(lead..).chain(trailing).collect();
    for message in volatile {
        let already = if message.images.is_empty() {
            history.iter().any(|m| m.role == message.role && m.images.is_empty() && m.tool_calls.is_empty()
                && m.tool_call_id.is_none() && m.content == message.content)
        } else {
            // Compare with the newest committed set OF THE SAME KIND: several
            // image messages can be live at once (an observation plus a
            // design reference board), and comparing against the newest of
            // any kind re-committed them alternately; comparing against any
            // older copy would leave a stale set looking current.
            history.iter().rev()
                .find(|m| !m.images.is_empty() && image_kind(m) == image_kind(&message))
                .is_some_and(|m| m.content == message.content && m.images == message.images)
        };
        if !already {
            history.push(message);
        }
    }
    prune_old_pixels(history);
    request.messages.extend(history.iter().cloned());
}

/// The producer of an image message, from its fixed leading label
/// ("NATIVE IMAGE SET", a design board header, …).
fn image_kind(message: &ChatMessage) -> &str {
    let line = message.content.lines().next().unwrap_or("");
    let end = line.find(|c: char| c == '—' || c == '.' || c == ':').unwrap_or(line.len());
    &line[..end]
}

fn prune_old_pixels(history: &mut [ChatMessage]) {
    let with_pixels: Vec<usize> = history.iter().enumerate()
        .filter(|(_, m)| !m.images.is_empty()).map(|(i, _)| i).collect();
    if with_pixels.len() <= PIXEL_MESSAGES_KEPT * 2 {
        return;
    }
    for &index in &with_pixels[..with_pixels.len() - PIXEL_MESSAGES_KEPT] {
        let message = &mut history[index];
        message.images.clear();
        message.content = format!(
            "[Earlier image set — pixels no longer attached; rely on the observations you recorded when you inspected it.]\n{}",
            message.content
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(notes: &[&str]) -> CompletionRequest {
        let mut messages = vec![ChatMessage::system("rules"), ChatMessage::user("task")];
        messages.extend(notes.iter().map(|n| ChatMessage::system(*n)));
        CompletionRequest::new("m", messages)
    }

    #[test]
    fn each_request_extends_the_previous_one_exactly() {
        let mut history = Vec::new();
        let mut first = request(&["FOLLOW-UP", "todo v1"]);
        commit_round(&mut first, &mut history, vec![]);
        history.push(ChatMessage::tool_result("c1", "out"));
        let mut second = request(&["FOLLOW-UP", "todo v1"]);
        commit_round(&mut second, &mut history, vec![]);
        let texts = |r: &CompletionRequest| r.messages.iter().map(|m| m.content.clone()).collect::<Vec<_>>();
        let (a, b) = (texts(&first), texts(&second));
        assert_eq!(b[..a.len()], a[..], "previous request must be a prefix");
        assert_eq!(b.len(), a.len() + 1, "unchanged notes are not re-sent");
        let mut third = request(&["FOLLOW-UP", "todo v2"]);
        commit_round(&mut third, &mut history, vec![]);
        assert_eq!(texts(&third)[..b.len()], b[..]);
        assert_eq!(texts(&third).last().unwrap(), "todo v2");
    }

    #[test]
    fn alternating_live_image_sets_are_each_committed_once() {
        let mut history = Vec::new();
        let observation = ChatMessage::user_with_images("observation", vec!["data:obs".into()]);
        let board = ChatMessage::user_with_images("reference board", vec!["data:a".into(), "data:b".into()]);
        for _ in 0..4 {
            let mut r = request(&[]);
            commit_round(&mut r, &mut history, vec![observation.clone(), board.clone()]);
        }
        assert_eq!(history.len(), 2);
    }

    #[test]
    fn image_sets_commit_once_and_old_pixels_are_pruned_in_batches() {
        let mut history = Vec::new();
        for round in 0..9 {
            let image = ChatMessage::user_with_images(format!("set {round}"), vec![format!("data:{round}")]);
            let mut r = request(&[]);
            commit_round(&mut r, &mut history, vec![image.clone()]);
            let mut again = request(&[]);
            commit_round(&mut again, &mut history, vec![image]);
            assert_eq!(r.messages.len(), again.messages.len(), "identical image set re-sent");
        }
        let with_pixels = history.iter().filter(|m| !m.images.is_empty()).count();
        assert!((PIXEL_MESSAGES_KEPT..=PIXEL_MESSAGES_KEPT * 2).contains(&with_pixels), "{with_pixels}");
        assert!(history[0].content.starts_with("[Earlier image set"));
    }
}
