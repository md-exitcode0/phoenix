//! Normalize configured model IDs for provider API calls and CLI display.

/// Model string sent to the provider API (no duplicate provider prefix).
pub fn api_model_id(_provider_id: &str, model: &str) -> String {
    model.trim().to_string()
}

/// Human-readable provider/model line for prompts and traces.
pub fn display_provider_model(provider_id: Option<&str>, model: &str) -> String {
    let model = model.trim();
    match provider_id {
        Some(provider) if model.starts_with(&format!("{provider}/")) => model.to_string(),
        Some(provider) => format!("{provider}/{model}"),
        None => model.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_avoids_duplicate_provider_prefix() {
        assert_eq!(
            display_provider_model(Some("openrouter"), "openrouter/auto"),
            "openrouter/auto"
        );
        assert_eq!(
            display_provider_model(Some("openrouter"), "deepseek/deepseek-v4-flash:free"),
            "openrouter/deepseek/deepseek-v4-flash:free"
        );
    }
}
