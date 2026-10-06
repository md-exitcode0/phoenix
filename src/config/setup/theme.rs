//! Wizard styling (onboard / configure / any prompt outside the TUI).
//! The same design language as the TUI: one warm grey ramp + phoenix
//! orange for anything interactive; full color only where the user acts.

/// The flame-gradient wordmark, one line: `▲ P H O E N I X` — the whole
/// identity, no ASCII-art animation.
pub fn wordmark(subtitle: &str) -> String {
    // Truecolor flame ramp, mirroring the TUI header's FLAME constant.
    const FLAME: [(u8, u8, u8); 5] = [
        (255, 70, 0),
        (255, 110, 0),
        (255, 150, 0),
        (255, 195, 0),
        (255, 235, 90),
    ];
    let mut out = String::from("  ");
    let (r, g, b) = FLAME[0];
    out.push_str(&format!("\x1b[38;2;{r};{g};{b}m▲\x1b[0m "));
    for (i, ch) in "PHOENIX".chars().enumerate() {
        let (r, g, b) = FLAME[(i * FLAME.len()) / 7];
        out.push_str(&format!("\x1b[1;38;2;{r};{g};{b}m{ch}\x1b[0m "));
    }
    if !subtitle.is_empty() {
        out.push_str(&format!("\x1b[2m {subtitle}\x1b[0m"));
    }
    out
}

/// Shared dialoguer theme for every interactive prompt Phoenix asks outside
/// the TUI. Orange lives on the prompt glyph, the hovered row, and the fuzzy
/// cursor; everything settled drops to the grey ramp — same "color = alive"
/// rule as the TUI feed.
pub fn phoenix_theme() -> dialoguer::theme::ColorfulTheme {
    use console::Style;
    let accent = Style::new().for_stderr().color256(208); // phoenix orange
    let grey = Style::new().for_stderr().color256(246);
    let dim = Style::new().for_stderr().color256(242);
    dialoguer::theme::ColorfulTheme {
        prompt_prefix: accent.clone().bold().apply_to("❯".to_string()),
        prompt_suffix: dim.clone().apply_to("·".to_string()),
        prompt_style: Style::new().for_stderr().color256(254),
        success_prefix: Style::new()
            .for_stderr()
            .color256(71)
            .apply_to("✔".to_string()),
        success_suffix: dim.clone().apply_to("·".to_string()),
        error_prefix: Style::new()
            .for_stderr()
            .color256(167)
            .apply_to("✘".to_string()),
        error_style: Style::new().for_stderr().color256(167),
        hint_style: dim.clone(),
        values_style: grey.clone(),
        defaults_style: dim.clone(),
        active_item_prefix: accent.clone().apply_to("▸".to_string()),
        inactive_item_prefix: Style::new().for_stderr().apply_to(" ".to_string()),
        active_item_style: accent.clone().bold(),
        inactive_item_style: grey,
        checked_item_prefix: accent.clone().apply_to("◉".to_string()),
        unchecked_item_prefix: dim.apply_to("◯".to_string()),
        picked_item_prefix: accent.clone().apply_to("▸".to_string()),
        unpicked_item_prefix: Style::new().for_stderr().apply_to(" ".to_string()),
        fuzzy_cursor_style: accent.clone(),
        fuzzy_match_highlight_style: accent.bold(),
    }
}
