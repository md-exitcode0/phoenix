//! Speech-to-text shared by chat apps (voice messages) and the
//! `transcribe_audio` tool. Uses the configured STT lane
//! (`[profile.llm] stt_provider` / `stt_model`, the same lane as the desktop
//! microphone); with no lane but a Groq key it uses Groq's free Whisper.
use anyhow::{bail, Context, Result};
use std::time::Duration;

/// Largest audio accepted in one request (Groq and OpenAI both cap at 25 MB).
pub const MAX_AUDIO_BYTES: usize = 25 * 1024 * 1024;
const GROQ_DEFAULT_MODEL: &str = "whisper-large-v3-turbo";

fn api_base(provider: &str) -> Result<&'static str> {
    match provider {
        "groq" => Ok("https://api.groq.com/openai/v1"),
        "openai" => Ok("https://api.openai.com/v1"),
        other => bail!("Speech-to-text is not available for provider `{other}`; use Groq or OpenAI"),
    }
}

fn provider_key(provider: &str) -> Option<String> {
    let env = match provider {
        "groq" => "GROQ_API_KEY",
        "openai" => "OPENAI_API_KEY",
        _ => return None,
    };
    if let Some(value) = std::env::var(env).ok().filter(|v| !v.trim().is_empty()) {
        return Some(value);
    }
    let raw = std::fs::read(crate::config::phoenix_home().join("auth-profiles.json")).ok()?;
    let doc: serde_json::Value = serde_json::from_slice(&raw).ok()?;
    doc["profiles"].as_object()?.values().find_map(|entry| {
        (entry["provider"].as_str() == Some(provider))
            .then(|| entry.get("key").or_else(|| entry.get("access")))
            .flatten()
            .and_then(|v| v.as_str())
            .filter(|v| !v.trim().is_empty())
            .map(str::to_string)
    })
}

/// (provider, model) for speech-to-text, or a message saying how to set it up.
fn lane() -> Result<(String, String)> {
    let configured = std::fs::read_to_string(crate::config::phoenix_home().join("config.toml"))
        .ok()
        .and_then(|raw| raw.parse::<toml::Value>().ok())
        .and_then(|doc| {
            let llm = doc.get("profile")?.get("llm")?;
            let get = |key: &str| llm.get(key).and_then(|v| v.as_str()).map(str::trim).filter(|v| !v.is_empty()).map(str::to_string);
            Some((get("stt_provider")?, get("stt_model")?))
        });
    if let Some((provider, model)) = configured {
        // A chat model picked for this lane cannot transcribe; use the
        // provider's own speech model instead of failing every request.
        let speech = model.contains("whisper") || model.contains("transcribe");
        let fallback = match provider.as_str() {
            "groq" => Some(GROQ_DEFAULT_MODEL),
            "openai" => Some("gpt-4o-mini-transcribe"),
            _ => None,
        };
        return Ok(match (speech, fallback) {
            (false, Some(default)) => (provider, default.to_string()),
            _ => (provider, model),
        });
    }
    if provider_key("groq").is_some() {
        return Ok(("groq".into(), GROQ_DEFAULT_MODEL.into()));
    }
    bail!("Speech-to-text is not set up. Add a (free) Groq API key in Phoenix → Settings → Models & Providers, then voice messages and audio files are transcribed automatically.")
}

/// Transcribes one audio or video file's speech. `filename` only needs the
/// right extension (ogg, oga, mp3, m4a, wav, webm, mp4…).
pub async fn transcribe(bytes: Vec<u8>, filename: &str) -> Result<String> {
    anyhow::ensure!(!bytes.is_empty(), "The audio file is empty");
    anyhow::ensure!(bytes.len() <= MAX_AUDIO_BYTES, "The audio is larger than 25 MB; split it into shorter parts first");
    let (provider, model) = lane()?;
    let key = provider_key(&provider).with_context(|| format!("No API key for `{provider}`. Add it in Settings → Models & Providers."))?;
    let part = reqwest::multipart::Part::bytes(bytes).file_name(filename.to_string());
    let form = reqwest::multipart::Form::new().text("model", model).part("file", part);
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(180))
        .build()?;
    let response = client
        .post(format!("{}/audio/transcriptions", api_base(&provider)?))
        .bearer_auth(key)
        .multipart(form)
        .send()
        .await
        .map_err(|_| anyhow::anyhow!("Could not reach the speech-to-text service"))?;
    let status = response.status();
    if !status.is_success() {
        // The provider's own reason (never the request or key) makes the
        // failure fixable: wrong model, unsupported file type, quota…
        let reason = response.json::<serde_json::Value>().await.ok()
            .and_then(|body| body["error"]["message"].as_str().map(|m| m.chars().take(240).collect::<String>()))
            .unwrap_or_default();
        bail!("Speech-to-text failed (HTTP {}){}{}", status.as_u16(), if reason.is_empty() { "" } else { ": " }, reason);
    }
    let value: serde_json::Value = response.json().await.context("Speech-to-text returned an unreadable answer")?;
    value["text"]
        .as_str()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
        .context("No speech was recognised in the audio")
}
