//! Free helpers: colors, truncation, session-file peeking, markdown
//! rendering, and input-box geometry.

use super::*;
use anyhow::Context as _;
use std::io::Read as _;

const MAX_TUI_HISTORY_BYTES: u64 = 1024 * 1024;
const MAX_TUI_HISTORY_ENTRY_BYTES: usize = 64 * 1024;
const MAX_TUI_SESSION_BYTES: u64 = 16 * 1024 * 1024;
const MAX_TUI_ARCHIVE_STATUS_BYTES: u64 = 64 * 1024 * 1024;

fn open_bounded_regular_file(
    path: &std::path::Path,
    max_bytes: u64,
) -> anyhow::Result<Option<(std::fs::File, u64)>> {
    crate::config::private_io::reject_symlink_components(path)?;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to open {}", path.display()));
        }
    };
    let metadata = file
        .metadata()
        .with_context(|| format!("failed to inspect {}", path.display()))?;
    if !metadata.is_file() {
        anyhow::bail!("{} is not a regular file", path.display());
    }
    if metadata.len() > max_bytes {
        anyhow::bail!(
            "{} is {} bytes; maximum is {max_bytes}",
            path.display(),
            metadata.len()
        );
    }
    Ok(Some((file, metadata.len())))
}

fn read_bounded_regular_file(
    path: &std::path::Path,
    max_bytes: u64,
) -> anyhow::Result<Option<Vec<u8>>> {
    let Some((file, expected_len)) = open_bounded_regular_file(path, max_bytes)? else {
        return Ok(None);
    };
    let mut bytes = Vec::with_capacity(expected_len as usize);
    file.take(max_bytes + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("failed to read {}", path.display()))?;
    if bytes.len() as u64 > max_bytes {
        anyhow::bail!(
            "{} grew beyond {max_bytes} bytes while reading",
            path.display()
        );
    }
    Ok(Some(bytes))
}

pub(super) fn agent_color(raw: &str) -> Color {
    match agent_key(raw) {
        "orchestrator" => ACCENT,
        "planner" => Color::Rgb(170, 210, 90),
        "coder" => Color::Rgb(80, 180, 255),
        "researcher" => Color::Rgb(190, 140, 255),
        "browser" => Color::Rgb(80, 220, 180),
        "frontend" => Color::Rgb(255, 120, 200),
        "database" => Color::Rgb(120, 160, 255),
        "hacker" => Color::Rgb(255, 90, 90),
        "critic" => Color::Rgb(255, 200, 60),
        "tester" => Color::Rgb(90, 210, 140),
        "presentation" => Color::Rgb(255, 170, 90),
        "computer_use" => Color::Rgb(140, 230, 255),
        _ => DIM,
    }
}

/// Full user-facing agent name — `Leo (coder)` — for activity rows and
/// return blocks. `·` (anonymous runtime rows) stays blank. Labels that are
/// already display names pass through; postbox instance labels ("coder#2")
/// render with their instance number preserved ("Leo (coder) #2").
pub(super) fn short_agent(raw: &str) -> String {
    if raw == "·" {
        return "".to_string();
    }
    if raw.contains('(') {
        return raw.to_string();
    }
    crate::runtime::delegation::agent_display_name(raw)
}

/// Total size in bytes of every file under a directory (0 when absent) —
/// skips the `models/` cache (the ONNX embedder, not memory content).
pub(super) fn walkdir_size(root: &std::path::Path) -> u64 {
    let mut total = 0u64;
    let mut walk = vec![root.to_path_buf()];
    while let Some(dir) = walk.pop() {
        if dir.file_name().is_some_and(|n| n == "models") {
            continue;
        }
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk.push(path);
                } else if let Ok(meta) = entry.metadata() {
                    total += meta.len();
                }
            }
        }
    }
    total
}

pub(super) fn truncate(text: &str, max: usize) -> String {
    let clean = text.replace('\n', " ");
    if clean.chars().count() <= max {
        clean
    } else {
        let cut: String = clean.chars().take(max).collect();
        format!("{cut}…")
    }
}

pub(super) fn first_sentence(text: &str, max: usize) -> String {
    let clean = text.replace('\n', " ").trim().to_string();
    let end = clean.find(". ").map(|i| i + 1).unwrap_or(clean.len());
    truncate(&clean[..end], max)
}

pub(super) fn read_session(path: &std::path::Path) -> Option<crate::session::Session> {
    let bytes = match read_bounded_regular_file(path, MAX_TUI_SESSION_BYTES) {
        Ok(Some(bytes)) => bytes,
        Ok(None) => return None,
        Err(error) => {
            tracing::warn!(
                path = %path.display(),
                error = %format_args!("{error:#}"),
                "TUI refused unreadable session file"
            );
            return None;
        }
    };
    match serde_json::from_slice(&bytes) {
        Ok(session) => Some(session),
        Err(error) => {
            tracing::warn!(
                path = %path.display(),
                error = %error,
                "TUI refused corrupt session file"
            );
            None
        }
    }
}

pub(super) fn load_history(path: &std::path::Path) -> anyhow::Result<Vec<String>> {
    let Some(bytes) = read_bounded_regular_file(path, MAX_TUI_HISTORY_BYTES)? else {
        return Ok(Vec::new());
    };
    let content = String::from_utf8(bytes)
        .with_context(|| format!("history is not UTF-8: {}", path.display()))?;
    Ok(content
        .lines()
        .filter(|line| !line.trim().is_empty() && !line.starts_with('#'))
        .map(str::to_string)
        .collect())
}

pub(super) fn persist_history(path: &std::path::Path, history: &[String]) -> anyhow::Result<()> {
    let mut newest_first = Vec::new();
    let mut remaining = MAX_TUI_HISTORY_BYTES as usize;
    for entry in history.iter().rev().take(500) {
        if remaining <= 1 {
            break;
        }
        let entry = entry.trim_end_matches(|character| character == '\r' || character == '\n');
        let mut end = entry
            .len()
            .min(MAX_TUI_HISTORY_ENTRY_BYTES)
            .min(remaining - 1);
        while !entry.is_char_boundary(end) {
            end -= 1;
        }
        if end == 0 {
            continue;
        }
        newest_first.push(entry[..end].to_string());
        remaining -= end + 1;
    }
    newest_first.reverse();
    let mut content = newest_first.join("\n");
    if !content.is_empty() {
        content.push('\n');
    }
    crate::config::private_io::atomic_write_private(path, content.as_bytes())
        .with_context(|| format!("failed to save TUI history {}", path.display()))
}

pub(super) fn session_file_len(path: &std::path::Path) -> Option<u64> {
    match open_bounded_regular_file(path, MAX_TUI_SESSION_BYTES) {
        Ok(Some((_file, len))) => Some(len),
        Ok(None) => None,
        Err(error) => {
            tracing::warn!(
                path = %path.display(),
                error = %format_args!("{error:#}"),
                "TUI refused unsafe session metadata"
            );
            None
        }
    }
}

/// A session's legible name: its stored title, else one derived from its first
/// user message (so sessions saved before titles existed are still readable),
/// else None.
pub(super) fn session_title(path: &std::path::Path) -> Option<String> {
    let session = read_session(path)?;
    if let Some(title) = session.title.filter(|t| !t.trim().is_empty()) {
        return Some(title);
    }
    session.messages.iter().find_map(|m| match m {
        crate::session::Message::User { content } => {
            Some(crate::session::derive_session_title(content))
        }
        _ => None,
    })
}

pub(super) fn session_message_counts(path: &std::path::Path) -> Option<(usize, usize)> {
    let session = read_session(path)?;
    let tool_results = session
        .messages
        .iter()
        .filter(|message| matches!(message, crate::session::Message::ToolResult { .. }))
        .count();
    Some((session.messages.len(), tool_results))
}

/// The `[Reasoning]…[/Reasoning]` block the mesh prepends to assistant
/// transcript entries — shown as a dim one-liner on replay.
pub(super) fn extract_reasoning(content: &str) -> Option<String> {
    let start = content.find("[Reasoning]")? + "[Reasoning]".len();
    let end = content.find("[/Reasoning]")?;
    if end <= start {
        return None;
    }
    let text = content[start..end].replace('\n', " ").trim().to_string();
    // The runtime's no-prose placeholder is plumbing, not narration.
    if text.is_empty() || text.starts_with("Provider requested native tool call") {
        None
    } else {
        Some(text)
    }
}

/// On-disk transcript path, mirroring the runner's state-root rule: the
/// Phoenix home IS the state root when it is named `.phoenix`; otherwise the
/// runner nests a `.phoenix` directory inside it (PHOENIX_HOME overrides).
pub(super) fn state_root() -> std::path::PathBuf {
    let home = crate::config::phoenix_home();
    if home.file_name().and_then(|n| n.to_str()) == Some(".phoenix") {
        home
    } else {
        home.join(".phoenix")
    }
}

pub(super) fn session_file(session_id: &str) -> std::path::PathBuf {
    state_root()
        .join("sessions")
        .join(format!("{session_id}.json"))
}

pub(super) fn session_archive_file(session_id: &str) -> std::path::PathBuf {
    state_root()
        .join("sessions")
        .join(format!("{session_id}.archive.jsonl"))
}

pub(super) fn context_archive_status_line(session_id: &str) -> Option<String> {
    let path = session_archive_file(session_id);
    context_archive_status_line_for_path(&path)
}

pub(super) fn context_archive_status_line_for_path(path: &std::path::Path) -> Option<String> {
    let bytes = match read_bounded_regular_file(path, MAX_TUI_ARCHIVE_STATUS_BYTES) {
        Ok(Some(bytes)) => bytes,
        Ok(None) => return None,
        Err(error) => {
            tracing::warn!(
                path = %path.display(),
                error = %format_args!("{error:#}"),
                "TUI refused unreadable context archive"
            );
            return None;
        }
    };
    let archived_messages = bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| line.iter().any(|byte| !byte.is_ascii_whitespace()))
        .count();

    Some(format!(
        "context archive · {} archived folded messages · size {} · {}",
        format_count(archived_messages as u64),
        format_bytes(bytes.len() as u64),
        path.display()
    ))
}

/// Pull the user-visible answer out of a transcript assistant message.
/// Mesh format: optional `[Reasoning]…[/Reasoning]` then
/// `Native tool request: final_answer({ … "final_markdown": "…" … })`.
pub(super) fn extract_final_markdown(content: &str) -> Option<String> {
    let candidate = if let Some(idx) = content.find("final_answer(") {
        let rest = &content[idx..];
        let open = rest.find('{')?;
        let close = rest.rfind('}')?;
        if close <= open {
            return None;
        }
        &rest[open..=close]
    } else {
        content.trim()
    };
    let value: serde_json::Value = serde_json::from_str(candidate).ok()?;
    value
        .get("final_markdown")
        .or_else(|| value.get("final").and_then(|f| f.get("final_markdown")))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .filter(|s| !s.trim().is_empty())
}

pub(super) fn byte_index(s: &str, char_index: usize) -> usize {
    s.char_indices()
        .nth(char_index)
        .map(|(i, _)| i)
        .unwrap_or(s.len())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct InputView {
    pub(super) lines: Vec<String>,
    pub(super) first_visible_line: usize,
    pub(super) cursor_row: usize,
    pub(super) cursor_col: usize,
}

/// Wrapped visual rows for `input` at `content_cols` — every logical line
/// (split on \n) hard-wraps at the border instead of scrolling off-screen. A
/// line that exactly fills its last row gets one empty continuation row so
/// the cursor has somewhere to sit.
fn wrapped_row_count(input: &str, content_cols: usize) -> usize {
    let cols = content_cols.max(1);
    input
        .split('\n')
        .map(|line| line.chars().count() / cols + 1)
        .sum()
}

pub(super) fn input_box_height(input: &str, terminal_height: u16, terminal_width: u16) -> u16 {
    const CHROME_ROWS: u16 = 7; // header + feed minimum + agent + status.
    const MIN_INPUT_ROWS: u16 = 3;
    const MAX_INPUT_ROWS: u16 = 7;

    let content_cols = terminal_width.saturating_sub(4).max(1) as usize;
    let rows = wrapped_row_count(input, content_cols).min(u16::MAX as usize) as u16;
    let desired = (rows + 2).clamp(MIN_INPUT_ROWS, MAX_INPUT_ROWS);
    let available = terminal_height
        .saturating_sub(CHROME_ROWS)
        .max(MIN_INPUT_ROWS);
    desired.min(available).max(MIN_INPUT_ROWS)
}

/// The input box viewport: logical lines SOFT-WRAP at the border (text
/// continues on the next visual row — no horizontal scrolling), and the
/// viewport scrolls vertically to keep the cursor's visual row in sight.
pub(super) fn input_view(
    input: &str,
    cursor: usize,
    area_width: u16,
    area_height: u16,
) -> InputView {
    let content_rows = area_height.saturating_sub(2).max(1) as usize;
    let content_cols = area_width.saturating_sub(4).max(1) as usize;

    let (cursor_line, cursor_col_raw) = cursor_line_col(input, cursor);
    let mut rows: Vec<String> = Vec::new();
    let mut cursor_visual_row = 0usize;
    let mut cursor_col = 0usize;
    for (line_index, line) in input.split('\n').enumerate() {
        let line = line.replace('\t', " ");
        let chars: Vec<char> = line.chars().collect();
        let chunk_count = chars.len() / content_cols + 1;
        if line_index == cursor_line {
            let col = cursor_col_raw.min(chars.len());
            let chunk = col / content_cols;
            cursor_visual_row = rows.len() + chunk;
            cursor_col = col - chunk * content_cols;
        }
        for chunk in 0..chunk_count {
            let start = chunk * content_cols;
            let end = ((chunk + 1) * content_cols).min(chars.len());
            rows.push(chars[start..end].iter().collect());
        }
    }
    if rows.is_empty() {
        rows.push(String::new());
    }
    let cursor_visual_row = cursor_visual_row.min(rows.len() - 1);

    let first_visible_line = (cursor_visual_row + 1).saturating_sub(content_rows);
    let last_visible = (first_visible_line + content_rows).min(rows.len());
    InputView {
        lines: rows[first_visible_line..last_visible].to_vec(),
        first_visible_line,
        cursor_row: cursor_visual_row - first_visible_line,
        cursor_col: cursor_col.min(content_cols),
    }
}

pub(super) fn cursor_line_col(input: &str, cursor: usize) -> (usize, usize) {
    let mut line = 0usize;
    let mut col = 0usize;
    for c in input.chars().take(cursor) {
        if c == '\n' {
            line += 1;
            col = 0;
        } else {
            col += 1;
        }
    }
    (line, col)
}

pub(super) fn format_count(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

pub(super) fn runtime_binary_status_line() -> String {
    let version = env!("CARGO_PKG_VERSION");
    let Ok(path) = std::env::current_exe() else {
        return format!("binary · phoenix {version} · current executable unknown");
    };
    let mut parts = vec![
        format!("binary · phoenix {version}"),
        path.display().to_string(),
    ];
    if let Ok(meta) = std::fs::metadata(&path) {
        parts.push(format!("size {}", format_bytes(meta.len())));
        if let Ok(modified) = meta.modified() {
            let local: DateTime<Local> = modified.into();
            parts.push(format!("modified {}", local.format("%Y-%m-%d %H:%M:%S")));
        }
    }
    if let Ok(hash) = short_file_sha256(&path) {
        parts.push(format!("sha256 {hash}"));
    }
    parts.join(" · ")
}

pub(super) fn short_file_sha256(path: &std::path::Path) -> io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 32 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let digest = hasher.finalize();
    Ok(digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

pub(super) fn format_bytes(bytes: u64) -> String {
    const MIB: f64 = 1024.0 * 1024.0;
    const KIB: f64 = 1024.0;
    if bytes >= 1024 * 1024 {
        format!("{:.1} MiB", bytes as f64 / MIB)
    } else if bytes >= 1024 {
        format!("{:.1} KiB", bytes as f64 / KIB)
    } else {
        format!("{bytes} B")
    }
}

pub(super) fn short_path(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.to_string())
        .unwrap_or_else(|| path.to_string())
}

/// Inline markdown within one line: `**bold**` brightens, `` `code` `` reads
/// monospace-on-surface. Unclosed markers render literally.
pub(super) fn inline_spans(text: &str, base: Style) -> Vec<Span<'static>> {
    let mut spans: Vec<Span> = Vec::new();
    let mut plain = String::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0usize;
    let flush = |plain: &mut String, spans: &mut Vec<Span>| {
        if !plain.is_empty() {
            spans.push(Span::styled(std::mem::take(plain), base));
        }
    };
    while i < chars.len() {
        if chars[i] == '*' && i + 1 < chars.len() && chars[i + 1] == '*' {
            if let Some(close) = (i + 2..chars.len().saturating_sub(1))
                .find(|&j| chars[j] == '*' && chars[j + 1] == '*')
            {
                flush(&mut plain, &mut spans);
                let inner: String = chars[i + 2..close].iter().collect();
                spans.push(Span::styled(
                    inner,
                    base.fg(TEXT).add_modifier(Modifier::BOLD),
                ));
                i = close + 2;
                continue;
            }
        }
        if chars[i] == '`' {
            if let Some(close) = (i + 1..chars.len()).find(|&j| chars[j] == '`') {
                flush(&mut plain, &mut spans);
                let inner: String = chars[i + 1..close].iter().collect();
                spans.push(Span::styled(
                    inner,
                    Style::default().fg(Color::Rgb(210, 190, 150)).bg(SURFACE),
                ));
                i = close + 1;
                continue;
            }
        }
        plain.push(chars[i]);
        i += 1;
    }
    flush(&mut plain, &mut spans);
    spans
}

/// Markdown styling for final answers: heading hierarchy, bullets, quotes,
/// rules, fenced code on a subtle surface, and inline bold/code. Tables stay
/// monospace as-is, which already reads well in a TUI.
pub(super) fn markdown_lines(markdown: &str) -> Vec<Line<'static>> {
    let body = Style::default().fg(TEXT);
    let mut lines = Vec::new();
    let mut in_code = false;
    for raw in markdown.lines() {
        let line = raw.to_string();
        if line.trim_start().starts_with("```") {
            in_code = !in_code;
            // The fence itself is scaffolding — a hairline, not content.
            lines.push(Line::from(Span::styled(
                format!("  {}", line.trim_start().trim_start_matches('`')),
                Style::default().fg(SUBTLE),
            )));
            continue;
        }
        if in_code {
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    line,
                    Style::default().fg(Color::Rgb(196, 204, 182)).bg(SURFACE),
                ),
            ]));
        } else if let Some(rest) = line.strip_prefix("### ") {
            let mut spans = vec![Span::raw("  ")];
            spans.extend(inline_spans(
                rest,
                Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
            ));
            lines.push(Line::from(spans));
        } else if let Some(rest) = line.strip_prefix("## ").or(line.strip_prefix("# ")) {
            lines.push(Line::raw(""));
            let mut spans = vec![Span::raw("  ")];
            spans.extend(inline_spans(
                rest,
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            ));
            lines.push(Line::from(spans));
        } else if let Some(rest) = line.strip_prefix("> ") {
            let mut spans = vec![Span::styled(
                "  ▏ ".to_string(),
                Style::default().fg(SUBTLE),
            )];
            spans.extend(inline_spans(
                rest,
                Style::default().fg(MID).add_modifier(Modifier::ITALIC),
            ));
            lines.push(Line::from(spans));
        } else if line.trim() == "---" {
            lines.push(Line::from(Span::styled(
                "  ──────────────────────".to_string(),
                Style::default().fg(SUBTLE),
            )));
        } else if let Some(rest) = line
            .strip_prefix("- ")
            .or(line.strip_prefix("* "))
            .or(line.trim_start().strip_prefix("- "))
        {
            let indent = line.len() - line.trim_start().len();
            let mut spans = vec![Span::styled(
                format!("  {}• ", " ".repeat(indent)),
                Style::default().fg(DIM),
            )];
            spans.extend(inline_spans(rest, body));
            lines.push(Line::from(spans));
        } else if let Some((num, rest)) = line
            .split_once(". ")
            .filter(|(num, _)| !num.is_empty() && num.chars().all(|c| c.is_ascii_digit()))
        {
            let mut spans = vec![Span::styled(format!("  {num}. "), Style::default().fg(DIM))];
            spans.extend(inline_spans(rest, body));
            lines.push(Line::from(spans));
        } else {
            let mut spans = vec![Span::raw("  ")];
            spans.extend(inline_spans(&line, body));
            lines.push(Line::from(spans));
        }
    }
    lines
}

/// Split a specialist transcript into per-delegation chunks: a new chunk
/// starts at every incoming brief (a Talk addressed to this agent, or a
/// direct user message). Resume replay splices chunk N under the N-th
/// hand-off row to that agent, so inner work reads where it happened.
pub(super) fn chunk_specialist_turns(
    messages: &[crate::session::Message],
    agent: &str,
) -> std::collections::VecDeque<Vec<crate::session::Message>> {
    let mut chunks = std::collections::VecDeque::new();
    let mut current: Vec<crate::session::Message> = Vec::new();
    for message in messages {
        let incoming = match message {
            crate::session::Message::Talk { to, .. } => agent_key(to) == agent_key(agent),
            crate::session::Message::User { .. } => true,
            _ => false,
        };
        if incoming && !current.is_empty() {
            chunks.push_back(std::mem::take(&mut current));
        }
        current.push(message.clone());
    }
    if !current.is_empty() {
        chunks.push_back(current);
    }
    chunks
}

#[cfg(test)]
mod chunk_tests {
    use super::chunk_specialist_turns;
    use crate::session::Message;

    fn talk(to: &str) -> Message {
        Message::Talk {
            from: "orchestrator".into(),
            to: to.into(),
            subject: "brief".into(),
            body: "do the thing".into(),
            reply_expected: false,
            handoff_id: String::new(),
            reply_to: None,
            causation_id: None,
            status: String::new(),
        }
    }

    fn tool() -> Message {
        Message::ToolResult {
            tool_name: "bash".into(),
            input: "{}".into(),
            success: true,
            output: "ok".into(),
        }
    }

    #[test]
    fn chunks_split_at_each_incoming_brief() {
        let messages = vec![
            talk("coder"),
            tool(),
            Message::Assistant {
                content: "done".into(),
            },
            talk("coder"),
            tool(),
        ];
        let chunks = chunk_specialist_turns(&messages, "coder");
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].len(), 3);
        assert_eq!(chunks[1].len(), 2);
    }

    #[test]
    fn display_name_labels_still_start_chunks() {
        // Hand-offs can carry "Leo (coder)" instead of the bare role.
        let messages = vec![talk("Leo (coder)"), tool(), talk("coder#2"), tool()];
        assert_eq!(chunk_specialist_turns(&messages, "coder").len(), 2);
    }
}

#[cfg(all(test, unix))]
mod bounded_state_io_tests {
    use super::*;
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::PermissionsExt;
    use std::time::Duration;

    #[test]
    fn history_read_rejects_symlink_fifo_and_oversize_file() {
        let dir = tempfile::tempdir().unwrap();

        let target = dir.path().join("target");
        std::fs::write(&target, "secret\n").unwrap();
        let linked = dir.path().join("linked-history");
        std::os::unix::fs::symlink(&target, &linked).unwrap();
        assert!(load_history(&linked).is_err());

        let fifo = dir.path().join("history.pipe");
        let fifo_c = CString::new(fifo.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) }, 0);
        let started = std::time::Instant::now();
        assert!(load_history(&fifo).is_err());
        assert!(started.elapsed() < Duration::from_secs(1));

        let oversize = dir.path().join("oversize-history");
        let file = std::fs::File::create(&oversize).unwrap();
        file.set_len(MAX_TUI_HISTORY_BYTES + 1).unwrap();
        assert!(load_history(&oversize).is_err());
    }

    #[test]
    fn session_read_rejects_symlink_fifo_and_oversize_file() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target.json");
        std::fs::write(&target, b"{}").unwrap();
        let linked = dir.path().join("linked.json");
        std::os::unix::fs::symlink(&target, &linked).unwrap();
        assert!(read_session(&linked).is_none());

        let fifo = dir.path().join("session.pipe");
        let fifo_c = CString::new(fifo.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) }, 0);
        let started = std::time::Instant::now();
        assert!(read_session(&fifo).is_none());
        assert!(started.elapsed() < Duration::from_secs(1));

        let oversize = dir.path().join("oversize.json");
        let file = std::fs::File::create(&oversize).unwrap();
        file.set_len(MAX_TUI_SESSION_BYTES + 1).unwrap();
        assert!(read_session(&oversize).is_none());
    }

    #[test]
    fn history_write_is_bounded_private_and_refuses_symlink_target() {
        let dir = tempfile::tempdir().unwrap();
        let history_path = dir.path().join("history");
        let history = vec!["old".into(), "x".repeat(MAX_TUI_HISTORY_BYTES as usize * 2)];
        persist_history(&history_path, &history).unwrap();
        let bytes = std::fs::read(&history_path).unwrap();
        assert!(bytes.len() <= MAX_TUI_HISTORY_BYTES as usize);
        assert_eq!(
            std::fs::metadata(&history_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );

        let victim = dir.path().join("victim");
        std::fs::write(&victim, "unchanged").unwrap();
        let linked = dir.path().join("linked");
        std::os::unix::fs::symlink(&victim, &linked).unwrap();
        assert!(persist_history(&linked, &["replacement".into()]).is_err());
        assert_eq!(std::fs::read_to_string(victim).unwrap(), "unchanged");
    }
}
