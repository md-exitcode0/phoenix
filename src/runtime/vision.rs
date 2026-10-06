//! Vision sidecar — a cheap image model that turns browser screenshots into
//! text for the core reasoning loop.
//!
//! Architecture decision (2026-06-09): the core model stays text-only; when
//! `vision_model` is configured, every successful `browser_screenshot` is also
//! described by the vision model and the description rides along in the tool
//! output. This buys screenshot grounding without multimodal plumbing through
//! every provider, and keeps per-step cost near zero (no image unless the
//! agent asks for a screenshot).
//!
//! Config (`~/.phoenix/config.toml`, `[profile.<name>.llm]`):
//!   vision_model    = "qwen/qwen2.5-vl-72b-instruct:free"   # enables vision
//!   vision_provider = "openrouter"                          # default: main provider
//!   native_vision   = true   # acting model is multimodal: attach screenshots
//!                            # as real images, skip the caption sidecar
//!                            # (OpenAI-compatible provider lanes only)

use std::path::Path;

use anyhow::{Context, Result};
use base64::Engine;
use serde_json::json;

use crate::config::auth_profile::resolve_llm_auth;
use crate::config::LLMProfile;
use crate::providers::providers_data;

/// Resolved endpoint + credential for the vision model. Built once at runner
/// setup; `None` when no `vision_model` is configured.
#[derive(Debug, Clone)]
pub struct VisionConfig {
    pub provider_id: String,
    pub model: String,
    pub base_url: String,
    pub api_key: Option<String>,
    /// Ordered, independently-authenticated candidates from the configured
    /// vision fallback chain. Live request failures advance through this list;
    /// startup auth failure promotes its first entry to primary.
    pub fallbacks: Vec<VisionConfig>,
}

/// What the vision model is asked about every screenshot — deterministic so
/// the caption is grounding, not a second agent improvising. Generic across
/// surfaces: the same hook captions browser pages, full desktops, and single
/// app windows.
const CAPTION_PROMPT: &str = "Describe this screenshot (a web page, desktop, or app window) for an automation agent: what app/page it is, key visible content and values, state of forms/buttons/menus, any dialogs, popups, cookie banners, CAPTCHAs, login walls, notifications, or error messages, and anything anomalous. Be specific and compact (under 150 words).";
const VISION_RESPONSE_MAX_BYTES: usize = 4 * 1024 * 1024;
const VISION_ERROR_PREVIEW_CHARS: usize = 500;
const VISION_PAGE_TEXT_MAX_BYTES: usize = 4 * 1024 * 1024;
const VISION_QUESTION_MAX_CHARS: usize = 16_384;

fn vision_error_preview(value: &str) -> String {
    let preview: String = value
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .take(VISION_ERROR_PREVIEW_CHARS)
        .collect();
    if value.chars().count() > VISION_ERROR_PREVIEW_CHARS {
        format!("{preview}...[truncated]")
    } else {
        preview
    }
}

async fn read_vision_json_response(
    response: reqwest::Response,
    label: &str,
) -> Result<serde_json::Value> {
    let status = response.status();
    let raw = crate::providers::read_response_text(response, VISION_RESPONSE_MAX_BYTES, label)
        .await
        .with_context(|| format!("failed to read {label} (HTTP {status})"))?;
    parse_vision_json(&raw, status, label)
}

fn parse_vision_json(
    raw: &str,
    status: reqwest::StatusCode,
    label: &str,
) -> Result<serde_json::Value> {
    let payload: serde_json::Value = serde_json::from_str(&raw).with_context(|| {
        format!(
            "{label} was not valid JSON (HTTP {status}): {}",
            vision_error_preview(&raw)
        )
    })?;
    if status.is_success() && payload.get("error").is_some_and(|error| !error.is_null()) {
        let detail = payload
            .pointer("/error/message")
            .or_else(|| payload.get("message"))
            .and_then(|value| value.as_str())
            .unwrap_or(raw);
        anyhow::bail!(
            "{label} reported an error despite HTTP {status}: {}",
            vision_error_preview(detail)
        );
    }
    if !status.is_success() {
        let detail = payload
            .pointer("/error/message")
            .or_else(|| payload.get("message"))
            .and_then(|value| value.as_str())
            .unwrap_or(&raw);
        anyhow::bail!(
            "{label} returned HTTP {status}: {}",
            vision_error_preview(detail)
        );
    }
    Ok(payload)
}

/// Build the vision config from the active LLM profile. Returns None (vision
/// disabled) when `vision_model` is unset; errors only on broken config.
pub fn vision_config(llm: &LLMProfile) -> Result<Option<VisionConfig>> {
    let Some(model) = llm.vision_model.clone().filter(|m| !m.trim().is_empty()) else {
        return Ok(None);
    };
    let provider_id = llm
        .vision_provider
        .clone()
        .filter(|p| !p.trim().is_empty())
        .unwrap_or_else(|| llm.provider.clone());
    let provider = providers_data::get_provider(&provider_id).with_context(|| {
        format!("vision_provider `{provider_id}` is not in the provider catalog")
    })?;
    // Cross-provider probe: drops the main lane's pinned auth so the vision
    // provider resolves its OWN credentials (the 2026-07-09 outage: the
    // ollama-cloud pin rode into codex resolution and failed every time).
    let probe = llm
        .probe_for_provider(&provider_id)
        .with_lane_pin("vision", &provider_id);
    match resolve_llm_auth(&probe, &provider, |name| std::env::var(name).ok()) {
        Ok(auth) => {
            let fallbacks = vision_chain_configs(llm, &model, &provider_id)
                .context("could not load the vision fallback credential store")?;
            Ok(Some(VisionConfig {
                provider_id,
                model,
                base_url: provider
                    .options
                    .base_url
                    .unwrap_or(provider.base_url)
                    .to_string(),
                api_key: auth.credential,
                fallbacks,
            }))
        }
        Err(primary_error) => {
            // Account fallback (plan 018): profiles chained on the vision
            // lane keep captions alive when the primary credential dies.
            let mut chain = vision_chain_configs(llm, &model, &provider_id)
                .context("could not load the vision fallback credential store")?;
            if !chain.is_empty() {
                let mut config = chain.remove(0);
                config.fallbacks = chain;
                eprintln!(
                    "warning: vision primary auth failed ({primary_error:#}) — using fallback profile lane {} ({})",
                    config.provider_id, config.model
                );
                return Ok(Some(config));
            }
            Err(primary_error).with_context(|| {
                format!("could not resolve auth for vision provider `{provider_id}`")
            })
        }
    }
}

/// First usable account in the vision fallback chain: its own credential and
/// (when assigned) its own vision model. A chain entry on the SAME provider
/// keeps the configured model; a different provider needs an explicit
/// assignment to know what to run, else it is skipped.
fn vision_chain_configs(
    llm: &LLMProfile,
    configured_model: &str,
    configured_provider: &str,
) -> Result<Vec<VisionConfig>> {
    if llm.fallback.vision.is_empty() {
        return Ok(vec![]);
    }
    let store = crate::config::auth_profile::load_auth_profile_store()?;
    let mut configs = Vec::new();
    for profile_id in &llm.fallback.vision {
        let Some(credential) = store.profiles.get(profile_id) else {
            continue;
        };
        let provider_id = crate::config::auth_profile::profile_provider_id(credential).to_string();
        let Some(provider) = providers_data::get_provider(&provider_id) else {
            continue;
        };
        let model = match store.assignment_for(profile_id, "vision") {
            Some(assignment) => assignment.model,
            None if provider_id == configured_provider => configured_model.to_string(),
            None => continue,
        };
        let Ok(secret) = crate::config::auth_profile::extract_profile_secret(credential) else {
            continue;
        };
        configs.push(VisionConfig {
            provider_id,
            model,
            base_url: provider
                .options
                .base_url
                .unwrap_or(provider.base_url)
                .to_string(),
            api_key: Some(secret),
            fallbacks: vec![],
        });
    }
    Ok(configs)
}

/// Deterministic page-text compaction: collapse blank-line runs, trim line
/// edges, and dedupe repeated lines (a page's nav/footer/action-row
/// boilerplate repeats dozens of times in `innerText`; substance rarely
/// repeats verbatim). Every UNIQUE line survives — this is compression, not
/// truncation. Short lines (≤3 chars: bullets, counters, separators) are
/// exempt from deduping so lists keep their shape.
pub(crate) fn compact_page_text(page_text: &str) -> String {
    let mut seen: std::collections::HashMap<&str, u32> = std::collections::HashMap::new();
    let mut out: Vec<String> = Vec::new();
    let mut dropped = 0usize;
    let mut blank_run = 0usize;
    for raw in page_text.lines() {
        let line = raw.trim();
        if line.is_empty() {
            blank_run += 1;
            if blank_run == 1 {
                out.push(String::new());
            }
            continue;
        }
        blank_run = 0;
        if line.chars().count() > 3 {
            let count = seen.entry(line).or_insert(0);
            *count += 1;
            // A line is allowed twice (headers legitimately echo once);
            // the third+ copy is boilerplate.
            if *count > 2 {
                dropped += 1;
                continue;
            }
        }
        out.push(line.to_string());
    }
    let mut text = out.join("\n");
    if dropped > 0 {
        text.push_str(&format!(
            "\n\n[{dropped} repeated boilerplate line(s) deduplicated — all unique content above]"
        ));
    }
    text
}

/// Structure a page extraction with the sidecar model (donor: browser-use's
/// LLM `extract` with `page_extraction_llm`). The model answers the query
/// strictly from the supplied page text.
pub async fn structure_extract(
    config: &VisionConfig,
    page_text: &str,
    query: &str,
) -> Result<String> {
    const INSTRUCTIONS: &str = "You extract exactly what is asked from web page text for an automation agent. Answer the query using ONLY the supplied text — quote values verbatim, keep structure compact (markdown lists/tables). For anything requested but absent, write NOT FOUND. No commentary.";
    anyhow::ensure!(
        page_text.len() <= VISION_PAGE_TEXT_MAX_BYTES,
        "page text exceeds the {VISION_PAGE_TEXT_MAX_BYTES}-byte extraction limit"
    );
    anyhow::ensure!(
        query.chars().take(VISION_QUESTION_MAX_CHARS + 1).count() <= VISION_QUESTION_MAX_CHARS,
        "extraction query exceeds the {VISION_QUESTION_MAX_CHARS}-character limit"
    );
    // Compress, don't truncate (donor lesson: vercel-labs/agent-browser feeds
    // models a compact structured snapshot, never a raw dump). innerText dumps
    // are dominated by REPETITION — nav menus, footers, cookie banners, "Reply
    // / Share / Save" rows repeated per item — and blank-line runs. Deduping
    // those keeps every unique line of substance while cutting the chars the
    // sidecar chews on (the 30–50s-per-extract latency measured 2026-07-04
    // scaled with input size).
    let text = compact_page_text(page_text);
    let prompt = format!("QUERY: {query}\n\nPAGE TEXT:\n{text}");
    let mut errors = Vec::new();
    for candidate in std::iter::once(config).chain(config.fallbacks.iter()) {
        match structure_extract_once(candidate, INSTRUCTIONS, &prompt).await {
            Ok(answer) => return Ok(answer),
            Err(error) => errors.push(format!(
                "{} ({}): {error:#}",
                candidate.provider_id, candidate.model
            )),
        }
    }
    anyhow::bail!("vision extraction chain exhausted: {}", errors.join(" | "))
}

async fn structure_extract_once(
    config: &VisionConfig,
    instructions: &str,
    prompt: &str,
) -> Result<String> {
    if config.provider_id == "openai-codex" {
        let token = config
            .api_key
            .clone()
            .context("extraction via openai-codex needs the OAuth access token")?;
        let provider = crate::providers::openai_codex::OpenAICodexProvider::with_url_and_timeout(
            config.base_url.clone(),
            token,
            std::time::Duration::from_secs(90),
        );
        return provider
            .complete_sidecar_text(&config.model, instructions, prompt)
            .await;
    }
    // The SuperGrok CLI proxy is streaming-only and needs its auth headers —
    // the raw chat/completions call below would 426. Route through the
    // provider, which handles the headers, streaming, and SSE assembly.
    if config.provider_id == "grok-cli" {
        return grok_cli_sidecar(config, instructions, prompt).await;
    }
    let body = json!({
        "model": config.model,
        "max_tokens": 2000,
        "messages": [
            {"role": "system", "content": instructions},
            {"role": "user", "content": prompt}
        ]
    });
    let url = chat_completions_url(config);
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(90))
        .build()?;
    let mut request = client.post(&url).json(&body);
    if let Some(key) = &config.api_key {
        request = request.bearer_auth(key);
    }
    let response = request.send().await.context("extraction request failed")?;
    let payload = read_vision_json_response(response, "extraction response").await?;
    payload
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|t| t.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .context("extraction response had no message content")
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    #[ignore = "two real configured vision calls; explicit before/after fixtures required"]
    async fn live_vision_distinguishes_cropped_and_complete_product() {
        let config = crate::config::PhoenixConfig::load().unwrap();
        let vision = super::vision_config(&config.profile.llm).unwrap().expect("configured vision route");
        let prompt = "Inspect product framing, not artistic taste. Is any physical part of the product cut off by the image boundary? Ignore cast shadows. Return only JSON with cropped (boolean), edge (string; none if not cropped), and evidence (string describing visible evidence). Do not assume that a polished render is correct.";
        for (variable, expected) in [("PHOENIX_VISION_COMPLETE_IMAGE", false), ("PHOENIX_VISION_CROPPED_IMAGE", true)] {
            let path = std::path::PathBuf::from(std::env::var(variable).expect("explicit fixture required"));
            assert!(path.is_absolute() && path.is_file());
            let before = std::fs::read(&path).unwrap();
            let answer = super::analyze_image_file(&vision, &path, Some(prompt)).await.unwrap();
            println!("PAIRED_FRAMING expected_cropped={expected} response={answer}");
            assert_eq!(std::fs::read(&path).unwrap(), before, "fixture must not change");
            let start = answer.find('{').expect("JSON response");
            let end = answer.rfind('}').unwrap();
            let result: serde_json::Value = serde_json::from_str(&answer[start..=end]).unwrap();
            assert_eq!(result["cropped"], expected);
            assert!(!result["evidence"].as_str().unwrap_or("").trim().is_empty());
            if expected {
                assert!(result["edge"].as_str().unwrap_or("").to_lowercase().contains("bottom"));
            }
        }
    }

    #[tokio::test]
    #[ignore = "real configured vision model; requires a known cropped fixture path"]
    async fn live_vision_rejects_cropped_product_confirmation() {
        let path = std::path::PathBuf::from(std::env::var("PHOENIX_VISION_ACCEPTANCE_IMAGE").expect("explicit fixture required"));
        assert!(path.is_absolute());
        let config = crate::config::PhoenixConfig::load().unwrap();
        let vision = super::vision_config(&config.profile.llm).unwrap().expect("configured vision route");
        let answer = super::analyze_image_file(&vision, &path, Some(
            "Confirm this is a clean product render with no clipping. Return only JSON with cropped (boolean), edge (string), and evidence (string). If the image contradicts that description, say so."
        )).await.unwrap();
        println!("VISION_FRAMING_RESULT={answer}");
        let start = answer.find('{').expect("JSON response");
        let end = answer.rfind('}').unwrap();
        let result: serde_json::Value = serde_json::from_str(&answer[start..=end]).unwrap();
        assert_eq!(result["cropped"], true, "known product extends below image boundary");
        assert!(result["edge"].as_str().unwrap_or("").to_lowercase().contains("bottom"));
    }

    use super::*;

    fn one_shot_vision_server(
        status: &str,
        body: &'static str,
    ) -> (String, std::thread::JoinHandle<()>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let status = status.to_string();
        let handle = std::thread::spawn(move || {
            use std::io::{Read, Write};
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 16 * 1024];
            let _ = stream.read(&mut request);
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).unwrap();
        });
        (format!("http://{address}/v1"), handle)
    }

    #[tokio::test]
    async fn live_request_failure_advances_through_vision_fallbacks() {
        let (failed_url, failed) = one_shot_vision_server(
            "503 Service Unavailable",
            r#"{"error":{"message":"primary unavailable"}}"#,
        );
        let (working_url, working) = one_shot_vision_server(
            "200 OK",
            r#"{"choices":[{"message":{"content":"fallback saw the image"}}]}"#,
        );
        let config = VisionConfig {
            provider_id: "primary-fixture".into(),
            model: "primary-model".into(),
            base_url: failed_url,
            api_key: None,
            fallbacks: vec![VisionConfig {
                provider_id: "fallback-fixture".into(),
                model: "fallback-model".into(),
                base_url: working_url,
                api_key: None,
                fallbacks: vec![],
            }],
        };

        let answer = ask_about_image(
            &config,
            "data:image/png;base64,AA==",
            "Describe the fixture",
        )
        .await
        .expect("the configured fallback should carry the request");
        assert_eq!(answer, "fallback saw the image");
        failed.join().unwrap();
        working.join().unwrap();

        let (failed_url, failed) = one_shot_vision_server(
            "429 Too Many Requests",
            r#"{"error":{"message":"primary rate limited"}}"#,
        );
        let (working_url, working) = one_shot_vision_server(
            "200 OK",
            r#"{"choices":[{"message":{"content":"fallback extracted the page"}}]}"#,
        );
        let extraction_config = VisionConfig {
            provider_id: "primary-fixture".into(),
            model: "primary-model".into(),
            base_url: failed_url,
            api_key: None,
            fallbacks: vec![VisionConfig {
                provider_id: "fallback-fixture".into(),
                model: "fallback-model".into(),
                base_url: working_url,
                api_key: None,
                fallbacks: vec![],
            }],
        };
        let extracted = structure_extract(&extraction_config, "Page total: $42", "Find the total")
            .await
            .expect("the configured fallback should carry extraction");
        assert_eq!(extracted, "fallback extracted the page");
        failed.join().unwrap();
        working.join().unwrap();
    }

    /// Live: the FULL vision path (analyze_image_file → ask_about_image →
    /// grok-cli provider) against the real SuperGrok proxy, using the token in
    /// ~/.grok/auth.json. Proves grok-4.5 vision works through Phoenix's own
    /// code, headers/streaming/SSE and all.
    ///   cargo test --lib runtime::vision::tests::live_grok -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn live_grok_cli_vision_reads_an_image() {
        // A simple two-panel PNG with both colors occupying half the image.
        // Large, balanced regions keep the live assertion about transport
        // and multimodal parsing deterministic across model revisions.
        use image::{Rgb, RgbImage};
        let mut img = RgbImage::from_pixel(256, 128, Rgb([30, 60, 200]));
        for y in 0..128 {
            for x in 128..256 {
                img.put_pixel(x, y, Rgb([220, 40, 40]));
            }
        }
        // A private, unpredictable directory prevents an existing /tmp
        // symlink from redirecting this live fixture into an arbitrary file.
        let dir = tempfile::tempdir().expect("private vision fixture directory");
        let path = dir.path().join("fixture.png");
        image::DynamicImage::ImageRgb8(img).save(&path).unwrap();

        let home = std::env::var("HOME").unwrap();
        let auth_path = std::path::PathBuf::from(home).join(".grok/auth.json");
        let raw = crate::config::private_io::read_private_file_limited(&auth_path, 256 * 1024)
            .expect("~/.grok/auth.json must be a safe bounded private file")
            .expect("~/.grok/auth.json — run `grok login`");
        let raw = String::from_utf8(raw).expect("~/.grok/auth.json must be UTF-8");
        let doc: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let token = doc.as_object().unwrap().values().next().unwrap()["key"]
            .as_str()
            .unwrap()
            .to_string();

        let config = VisionConfig {
            provider_id: "grok-cli".to_string(),
            model: "grok-4.5".to_string(),
            base_url: crate::providers::grok_cli::PROXY_BASE_URL.to_string(),
            api_key: Some(token),
            fallbacks: vec![],
        };
        let desc = analyze_image_file(
            &config,
            &path,
            Some("Name both dominant colors and say which half of the image each occupies."),
        )
        .await
        .expect("grok-cli vision failed");
        eprintln!("grok vision said: {desc}");
        let lower = desc.to_lowercase();
        assert!(
            lower.contains("blue") && lower.contains("red"),
            "got: {desc}"
        );
    }

    #[test]
    fn compact_page_text_dedupes_boilerplate_and_keeps_unique_content() {
        // A forum-like innerText dump: per-item action rows repeat, substance
        // doesn't. Compression must keep every unique line and drop only the
        // 3rd+ copy of repeats.
        let page = "Story one about rust\nReply Share Save\n\n\n\nStory two about tokio\nReply Share Save\nStory three about async\nReply Share Save\nReply Share Save\n42\n42\n42";
        let out = compact_page_text(page);
        assert!(out.contains("Story one about rust"));
        assert!(out.contains("Story two about tokio"));
        assert!(out.contains("Story three about async"));
        // "Reply Share Save" appeared 4x — exactly 2 survive.
        assert_eq!(out.matches("Reply Share Save").count(), 2);
        // Short lines (counters/bullets) are exempt from deduping.
        assert_eq!(out.matches("42").count(), 3);
        // Blank runs collapse to one.
        assert!(!out.contains("\n\n\n"));
        // The receipt names what happened.
        assert!(out.contains("deduplicated"), "{out}");
    }

    #[test]
    fn parse_point_handles_ui_tars_output_shapes() {
        assert_eq!(
            parse_point("click(point='<point>197 525</point>')"),
            Some((197, 525))
        );
        assert_eq!(
            parse_point("click(start_box='(197,525)')"),
            Some((197, 525))
        );
        assert_eq!(parse_point("The button is at (840, 12)."), Some((840, 12)));
        assert_eq!(parse_point("not_found"), None);
        assert_eq!(parse_point(""), None);
    }

    #[test]
    fn downscale_bounds_long_edge_and_reports_scale() {
        // A 3000x1000 screenshot must come back JPEG with the long edge at the
        // cap and a scale that maps encoded pixels to screen pixels.
        let wide = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            3000,
            1000,
            image::Rgb([120, 40, 200]),
        ));
        let mut png = Vec::new();
        wide.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let (jpeg, scale) = downscale_to_jpeg(&png).expect("downscale failed");
        let decoded = image::load_from_memory(&jpeg).unwrap();
        assert_eq!(decoded.width(), VISION_MAX_EDGE);
        assert!((scale - 3000.0 / VISION_MAX_EDGE as f64).abs() < 0.01);
        assert!(
            jpeg.len() < png.len(),
            "JPEG payload should be smaller than the PNG"
        );
        // A grounding point on the encoded image maps back to screen pixels.
        let screen_x = (720.0 * scale).round() as i64;
        assert_eq!(screen_x, 1500);
    }

    #[tokio::test]
    async fn native_screen_attachment_keeps_the_coordinate_grid_and_decode_limits() {
        use base64::Engine;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("desktop.png");
        // An odd height catches rounding changes as well as horizontal scaling.
        image::RgbImage::from_pixel(3000, 1001, image::Rgb([120, 40, 200]))
            .save(&path).unwrap();
        let native = native_screen_data_uri(&path).await.unwrap();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(native.split_once(',').unwrap().1).unwrap();
        let decoded = image::load_from_memory(&bytes).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (3000, 1001));
        let sidecar = vision_payload(&path).await.unwrap();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(sidecar.data_uri.split_once(',').unwrap().1).unwrap();
        let decoded = image::load_from_memory(&bytes).unwrap();
        assert_eq!(decoded.width(), VISION_MAX_EDGE);
        assert!(sidecar.scale > 2.0);
        image::RgbImage::from_pixel(VISION_DECODE_MAX_EDGE + 1, 1, image::Rgb([1,2,3]))
            .save(&path).unwrap();
        assert!(native_screen_data_uri(&path).await.is_err());
        std::fs::write(&path, b"not an image").unwrap();
        assert!(native_screen_data_uri(&path).await.is_err());
    }

    #[test]
    fn small_screenshots_are_not_upscaled() {
        let small = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            800,
            600,
            image::Rgb([10, 10, 10]),
        ));
        let mut png = Vec::new();
        small
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let (jpeg, scale) = downscale_to_jpeg(&png).expect("downscale failed");
        let decoded = image::load_from_memory(&jpeg).unwrap();
        assert_eq!(decoded.width(), 800);
        assert!((scale - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn undecodable_bytes_fall_back() {
        assert!(downscale_to_jpeg(b"not an image").is_none());
    }

    #[test]
    fn decoder_rejects_images_beyond_the_strict_dimension_limit() {
        let wide = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            VISION_DECODE_MAX_EDGE + 1,
            1,
            image::Rgb([1, 2, 3]),
        ));
        let mut png = Vec::new();
        wide.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        assert!(downscale_to_jpeg(&png).is_none());
        assert!(
            build_vision_payload(&png).is_err(),
            "strict decode failure must never fall back to raw oversized bytes"
        );
    }

    #[test]
    fn vision_payload_rejects_unrecognized_bytes_and_encodes_valid_png_as_jpeg() {
        assert!(build_vision_payload(b"not an image").is_err());

        let image = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            4,
            3,
            image::Rgb([1, 2, 3]),
        ));
        let mut png = Vec::new();
        image
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let payload = build_vision_payload(&png).unwrap();
        assert!(payload.data_uri.starts_with("data:image/jpeg;base64,"));
        assert!((payload.scale - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn passthrough_web_formats_keep_truthful_mime_and_bounded_dimensions() {
        let mut gif = b"GIF89a\x01\x00\x02\x00".to_vec();
        gif.extend_from_slice(b"\x00\x00\x00\x2c\x3b");
        let gif_payload = build_vision_payload(&gif).unwrap();
        assert!(gif_payload.data_uri.starts_with("data:image/gif;base64,"));

        let mut webp = vec![0u8; 44];
        webp[..4].copy_from_slice(b"RIFF");
        webp[4..8].copy_from_slice(&36u32.to_le_bytes());
        webp[8..12].copy_from_slice(b"WEBP");
        webp[12..16].copy_from_slice(b"VP8X");
        webp[16..20].copy_from_slice(&10u32.to_le_bytes());
        // VP8X stores canvas dimensions minus one in 24-bit little endian.
        webp[24] = 2; // width = 3
        webp[27] = 3; // height = 4
        webp[30..34].copy_from_slice(b"VP8L");
        webp[34..38].copy_from_slice(&5u32.to_le_bytes());
        webp[38] = 0x2f;
        let webp_payload = build_vision_payload(&webp).unwrap();
        assert!(webp_payload.data_uri.starts_with("data:image/webp;base64,"));

        webp[26] = 0x01; // width now exceeds the 8192 edge limit.
        assert!(build_vision_payload(&webp).is_err());
    }

    #[test]
    fn vision_json_errors_are_status_aware_and_bounded() {
        let raw = serde_json::json!({
            "error": {"message": format!("bad lane {}", "x".repeat(2_000))}
        })
        .to_string();
        let error = parse_vision_json(&raw, reqwest::StatusCode::BAD_GATEWAY, "fixture")
            .expect_err("non-success must fail")
            .to_string();
        assert!(error.contains("502"));
        assert!(error.contains("[truncated]"));
        assert!(error.chars().count() < 700);

        let invalid = parse_vision_json("not json", reqwest::StatusCode::OK, "fixture")
            .expect_err("invalid JSON must fail")
            .to_string();
        assert!(invalid.contains("invalid") || invalid.contains("not valid"));

        let logical = parse_vision_json(
            r#"{"error":{"message":"logical failure"}}"#,
            reqwest::StatusCode::OK,
            "fixture",
        )
        .expect_err("HTTP 200 error envelopes must fail")
        .to_string();
        assert!(logical.contains("logical failure"));
    }

    #[tokio::test]
    async fn oversized_vision_prompt_fails_before_network_io() {
        let config = VisionConfig {
            provider_id: "openrouter".into(),
            model: "fixture".into(),
            base_url: "https://example.invalid/v1".into(),
            api_key: None,
            fallbacks: vec![],
        };
        let error = ask_about_image(
            &config,
            "data:image/png;base64,AA==",
            &"x".repeat(VISION_QUESTION_MAX_CHARS + 1),
        )
        .await
        .expect_err("oversized prompt must fail");
        assert!(error.to_string().contains("prompt exceeds"));
    }

    #[cfg(unix)]
    #[test]
    fn vision_input_is_bounded_regular_and_nofollow() {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let safe = dir.path().join("safe.png");
        std::fs::write(&safe, b"small fixture").unwrap();
        assert_eq!(read_vision_image_bounded(&safe).unwrap(), b"small fixture");

        let oversized = dir.path().join("oversized.png");
        let file = std::fs::File::create(&oversized).unwrap();
        file.set_len(VISION_INPUT_MAX_BYTES + 1).unwrap();
        assert!(read_vision_image_bounded(&oversized).is_err());

        let linked = dir.path().join("linked.png");
        symlink(&safe, &linked).unwrap();
        assert!(read_vision_image_bounded(&linked).is_err());

        let fifo = dir.path().join("image.fifo");
        let fifo_name = CString::new(fifo.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo_name.as_ptr(), 0o600) }, 0);
        assert!(read_vision_image_bounded(&fifo).is_err());
    }

    #[test]
    fn disabled_without_vision_model() {
        let llm = LLMProfile::new("openrouter", "some-model");
        assert!(vision_config(&llm).expect("no error when unset").is_none());
    }

    #[test]
    fn unknown_vision_provider_is_an_error() {
        let mut llm = LLMProfile::new("openrouter", "some-model");
        llm.vision_model = Some("a-vision-model".to_string());
        llm.vision_provider = Some("not-a-real-provider".to_string());
        assert!(vision_config(&llm).is_err());
    }

    #[test]
    fn cross_provider_vision_drops_the_main_lanes_auth_pin() {
        // The 2026-07-09 outage: main lane pinned `auth.profile =
        // "ollama-cloud:2"`, vision pointed at openai-codex — the pin rode
        // into codex's resolution and failed EVERY time ("Auth profile
        // 'ollama-cloud:2' belongs to provider 'ollama-cloud' but config
        // expects 'openai-codex'"), surviving a codex re-login. The probe
        // must drop a cross-provider pin so codex resolves its own store.
        let mut llm = LLMProfile::new("ollama-cloud", "glm-5.2");
        llm.vision_model = Some("gpt-5.4-mini".to_string());
        llm.vision_provider = Some("openai-codex".to_string());
        llm.auth = Some(crate::config::LLMAuthConfig {
            method: None,
            source: Some("profile".to_string()),
            profile: Some("ollama-cloud:2".to_string()),
            env_var: None,
        });
        let vision = vision_config(&llm)
            .expect("cross-provider vision must not inherit the main auth pin")
            .expect("vision is configured");
        assert_eq!(vision.provider_id, "openai-codex");

        // Same-provider vision keeps the pin (it is valid and deliberate).
        let probe = llm.probe_for_provider("ollama-cloud");
        assert!(probe.auth.is_some());
        let cross = llm.probe_for_provider("openai-codex");
        assert!(cross.auth.is_none());
    }

    // Live extraction smoke test — real sidecar call.
    //   cargo test live_extract -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn live_extract_structures_page_text() {
        let cfg = crate::config::PhoenixConfig::load().expect("config");
        let vision = vision_config(&cfg.profile.llm)
            .expect("vision config")
            .expect("vision_model must be set in config");
        let page = "Acme Store\nWireless Mouse — $24.99 (in stock)\nUSB-C Hub — $59.00 (sold out)\nKeyboard — $89.50 (in stock)\nContact: support@acme.test";
        let out = structure_extract(
            &vision,
            page,
            "list each product with price and availability",
        )
        .await
        .expect("extract failed");
        println!("EXTRACTED:\n{out}");
        assert!(out.contains("24.99") && out.contains("59.00"), "{out}");
    }

    // Live vision smoke test — real provider call with a real screenshot.
    // Run explicitly:
    //   VISION_TEST_PNG=/tmp/vision-test.png cargo test live_vision -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn live_vision_describes_a_screenshot() {
        let png = std::env::var("VISION_TEST_PNG").expect("set VISION_TEST_PNG");
        let cfg = crate::config::PhoenixConfig::load().expect("config");
        let vision = vision_config(&cfg.profile.llm)
            .expect("vision config")
            .expect("vision_model must be set in config");
        let caption = describe_screenshot(&vision, Path::new(&png))
            .await
            .expect("vision call failed");
        println!("VISION CAPTION:\n{caption}");
        assert!(caption.len() > 20, "suspiciously short caption: {caption}");
    }
}

/// Describe a screenshot file; returns the caption text. Codex routes through
/// its Responses endpoint; everything else gets an OpenAI-compatible
/// chat/completions call with the image inlined as a data URI.
pub async fn describe_screenshot(config: &VisionConfig, png_path: &Path) -> Result<String> {
    ask_about_screenshot(config, png_path, CAPTION_PROMPT).await
}

/// The downscaled-JPEG data URI for a screenshot — the same payload the
/// sidecar sees, for attaching natively to a multimodal acting model
/// (`native_vision`). Needs no vision config: no sidecar call is made.
pub async fn screenshot_data_uri(png_path: &Path) -> Result<String> {
    Ok(vision_payload(png_path).await?.data_uri)
}

/// Prepared pixels and their source label. This is request state, never session
/// memory: only an explicit reference selection survives a new observation.
pub(crate) struct NativeImage {
    pub(crate) data_uri: String,
    label: String,
}

impl std::fmt::Debug for NativeImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeImage").field("label", &self.label)
            .field("encoded_bytes", &self.data_uri.len()).finish()
    }
}

impl NativeImage {
    pub(crate) fn file(path: &Path, crop: Option<ImageRegion>, data_uri: String) -> Self {
        let region = crop.map_or_else(|| "whole image".to_string(), |r| {
            format!("detail crop x={}, y={}, width={}, height={} in source pixels; not desktop coordinates", r.x, r.y, r.width, r.height)
        });
        Self { data_uri, label: format!("primary image {path:?} ({region}; file inspection, not necessarily the current desktop)") }
    }

    pub(crate) fn reference(path: &Path, data_uri: String) -> Self {
        Self { data_uri, label: format!("reference {path:?} (whole image; saved pixels explicitly selected by reference_paths for this turn)") }
    }

    pub(crate) fn screen(tool: &str, path: &Path, data_uri: String) -> Self {
        Self { data_uri, label: format!("current observation from {tool}: {path:?} (use the capture receipt's coordinate space)") }
    }
}

#[derive(Debug)]
pub(crate) struct NativeImageInspection {
    pub(crate) primary: NativeImage,
    pub(crate) references: Vec<NativeImage>,
}

/// At most one current observation and two explicit references, bounded by the
/// existing encoded-image limit in aggregate. No global cache or disk replay.
#[derive(Default)]
pub(crate) struct NativeTurnImages {
    current: Option<NativeImage>,
    references: Vec<NativeImage>,
    /// Earlier sets from the same tool batch, frozen so a second file
    /// inspection can proceed. The turn history is append-only, so they stay
    /// visible after the batch instead of being silently replaced.
    snapshots: Vec<crate::providers::ChatMessage>,
}

pub(crate) fn native_image_tool(tool: &str) -> bool {
    matches!(tool, "image_analyze" | "browser_screenshot" | "computer_screenshot"
        | "computer_act" | "computer_capture_window" | "computer_window_act"
        | "computer_locate" | "computer_read_text")
}

pub(crate) fn invalidates_native_observation(tool: &str) -> bool {
    native_image_tool(tool) || matches!(tool,
        "computer_move" | "computer_click" | "computer_drag" | "computer_scroll"
        | "computer_type" | "computer_key" | "computer_open"
        | "computer_focus_window" | "computer_lower_window"
        | "browser_act" | "browser_navigate" | "browser_search" | "browser_go_back"
        | "browser_click" | "browser_upload_file" | "browser_input" | "browser_input_credential"
        | "browser_send_keys" | "browser_scroll" | "browser_select_dropdown"
        | "browser_switch" | "browser_close" | "browser_evaluate" | "browser_import_cookies"
        // These may change the inspected file or rendered surface. Read-only
        // tools retain the observation; arbitrary shell work is conservative.
        | "write" | "str_replace" | "image_gen" | "bash")
}

impl NativeTurnImages {
    pub(crate) fn invalidate_current(&mut self) {
        self.current = None;
    }

    /// Commit a fully prepared comparison atomically. Empty reference_paths
    /// updates only the primary; it never promotes that primary to a reference.
    pub(crate) fn set_inspection(&mut self, images: NativeImageInspection) -> Result<()> {
        self.invalidate_current();
        let references = if images.references.is_empty() { &self.references } else { &images.references };
        Self::check_budget(Some(&images.primary), references)?;
        if !images.references.is_empty() {
            self.references = images.references;
        }
        self.current = Some(images.primary);
        Ok(())
    }

    pub(crate) fn set_current(&mut self, image: NativeImage) -> Result<&str> {
        self.set_inspection(NativeImageInspection { primary: image, references: Vec::new() })?;
        Ok(&self.current.as_ref().unwrap().data_uri)
    }

    pub(crate) fn has_current(&self) -> bool {
        self.current.is_some()
    }

    pub(crate) async fn capture(&mut self, tool: &str, path: &Path) -> Result<&str> {
        self.invalidate_current();
        let data_uri = native_screen_data_uri(path).await?;
        self.set_current(NativeImage::screen(tool, path, data_uri))
    }

    fn unique_images<'a>(current: Option<&'a NativeImage>, references: &'a [NativeImage]) -> Vec<&'a NativeImage> {
        let mut unique: Vec<&NativeImage> = Vec::new();
        for image in current.into_iter().chain(references.iter()) {
            // Compare exact encoded pixels, never filenames or approximate hashes.
            if !unique.iter().any(|seen| seen.data_uri == image.data_uri) {
                unique.push(image);
            }
        }
        unique
    }

    fn check_budget(current: Option<&NativeImage>, references: &[NativeImage]) -> Result<()> {
        anyhow::ensure!(references.len() <= 2, "At most two explicit reference images may be retained");
        let bytes = Self::unique_images(current, references).iter().try_fold(0usize, |total, image| {
            total.checked_add(image.data_uri.len()).context("native image set size overflow")
        })?;
        anyhow::ensure!(bytes <= VISION_DATA_URI_MAX_BYTES,
            "Native image set exceeds the {VISION_DATA_URI_MAX_BYTES}-byte aggregate encoded-image limit. No new observation or reference selection was accepted; prior references retain their original identity.");
        Ok(())
    }

    pub(crate) fn message(&self) -> Option<crate::providers::ChatMessage> {
        let unique = Self::unique_images(self.current.as_ref(), &self.references);
        if unique.is_empty() { return None; }
        let labels = self.current.iter().chain(self.references.iter()).map(|image| {
            let index = unique.iter().position(|seen| seen.data_uri == image.data_uri).unwrap() + 1;
            format!("{index} = {}", image.label)
        }).collect::<Vec<_>>().join("; ");
        let current_notice = if self.current.is_none() {
            "No current observation is attached. Retained references do not show current screen or artifact state; obtain a fresh observation before acting on coordinates or judging the current result."
        } else {
            "The primary/current observation is first. References retain their selected pixels, even if their source file has since changed. A crop covers only its labeled region."
        };
        Some(crate::providers::ChatMessage::user_with_images(
            format!("NATIVE IMAGE SET — {current_notice}\nAttached image order: {labels}\nRepeated labels with the same index share exactly the same attached pixels. Inspect the labeled pixels; do not claim inspection of other files."),
            unique.into_iter().map(|image| image.data_uri.clone()).collect(),
        ))
    }

    /// Freeze the currently prepared set so another inspection may replace it.
    pub(crate) fn snapshot_current(&mut self) {
        if let Some(message) = self.message() {
            self.snapshots.push(message);
        }
    }

    /// Frozen sets followed by the live one, in the order they were prepared.
    pub(crate) fn take_observations(&mut self) -> Vec<crate::providers::ChatMessage> {
        let mut out = std::mem::take(&mut self.snapshots);
        out.extend(self.message());
        out
    }

    pub(crate) fn append_to_request(&self, request: &mut crate::providers::CompletionRequest) {
        if let Some(message) = self.message() { request.messages.push(message); }
    }
}

#[cfg(test)]
mod native_turn_image_tests {
    use super::*;

    fn outgoing(images: &NativeTurnImages) -> crate::providers::CompletionRequest {
        let mut request = crate::providers::CompletionRequest::new("mock", vec![crate::providers::ChatMessage::user("Original brief")]);
        images.append_to_request(&mut request);
        request
    }

    #[tokio::test]
    async fn retained_reference_is_a_snapshot_after_its_source_changes() {
        let dir = tempfile::tempdir().unwrap();
        let reference = dir.path().join("input.png");
        image::RgbImage::from_pixel(32, 24, image::Rgb([10, 220, 10])).save(&reference).unwrap();
        let original = screenshot_data_uri(&reference).await.unwrap();
        let mut images = NativeTurnImages::default();
        crate::tools::image_analyze::attach_native(dir.path(), serde_json::from_value(json!({
            "path":"input.png", "reference_paths":["input.png"]
        })).unwrap(), &mut images).await.unwrap();
        // The same path now contains different pixels; retained selection must
        // remain an immutable comparison snapshot, not re-read silently.
        image::RgbImage::from_pixel(32, 24, image::Rgb([220, 10, 10])).save(&reference).unwrap();
        let changed = native_screen_data_uri(&reference).await.unwrap();
        for tool in ["computer_act", "computer_window_act", "browser_screenshot", "computer_capture_window", "computer_screenshot"] {
            assert!(invalidates_native_observation(tool));
            images.invalidate_current();
            assert!(images.message().unwrap().content.contains("No current observation"));
            images.capture(tool, &reference).await.unwrap();
            let request = outgoing(&images);
            let message = request.messages.last().unwrap();
            assert_eq!(message.images, vec![changed.clone(), original.clone()]);
            assert!(message.content.contains("even if their source file has since changed"));
            let wire = crate::providers::openai_codex::OpenAICodexProvider::responses_input(&request);
            let urls: Vec<_> = wire.iter().flat_map(|item| item["content"].as_array().into_iter().flatten())
                .filter_map(|part| part["image_url"].as_str()).collect();
            assert_eq!(urls, vec![changed.as_str(), original.as_str()]);
        }
        std::fs::write(&reference, b"broken").unwrap();
        assert!(images.capture("computer_screenshot", &reference).await.is_err());
        assert_eq!(images.message().unwrap().images, vec![original]);
        assert!(NativeTurnImages::default().message().is_none(), "a new turn/peer starts empty");
    }

    #[test]
    fn aggregate_image_budget_is_deduplicated_and_failure_is_atomic() {
        let image = |name, pixels: String| NativeImage::reference(Path::new(name), pixels);
        let mut images = NativeTurnImages::default();
        images.set_inspection(NativeImageInspection {
            primary: image("current", "current".into()), references: vec![image("kept", "kept".into())],
        }).unwrap();
        // Each candidate fits the individual limit; together they exceed the
        // aggregate. MIME and dimensions cannot bypass this encoded-byte bound.
        let too_large = NativeImageInspection {
            primary: image("new-current", "data:image/png;base64,AA==".into()),
            references: vec![image("new.gif", "g".repeat(VISION_DATA_URI_MAX_BYTES / 2)),
                image("new.webp", "w".repeat(VISION_DATA_URI_MAX_BYTES / 2))],
        };
        assert!(images.set_inspection(too_large).unwrap_err().to_string().contains("aggregate"));
        assert_eq!(images.message().unwrap().images, vec!["kept"]);
        assert!(!images.has_current());
        let duplicate = "x".repeat(VISION_DATA_URI_MAX_BYTES);
        images.set_inspection(NativeImageInspection {
            primary: image("same-current", duplicate.clone()),
            references: vec![image("same-ref", duplicate)],
        }).unwrap();
        let message = images.message().unwrap();
        assert_eq!(message.images.len(), 1);
        assert_eq!(message.images[0].len(), VISION_DATA_URI_MAX_BYTES);
        assert!(message.content.contains("1 = reference \"same-ref\""));
        assert!(images.set_current(image("different-current", "new".into())).is_err());
        assert!(!images.has_current());
        assert_eq!(images.message().unwrap().images.len(), 1);
        assert!(images.set_inspection(NativeImageInspection { primary:image("p", "p".into()),
            references:vec![image("a", "a".into()), image("b", "b".into()), image("c", "c".into())] }).is_err());
    }

    #[test]
    fn mutation_and_failed_refresh_invalidate_current_but_reads_do_not() {
        for tool in ["computer_window_act", "browser_click", "browser_evaluate", "write", "str_replace", "bash", "image_analyze", "computer_capture_window"] {
            assert!(invalidates_native_observation(tool), "{tool}");
        }
        for tool in ["read", "recall", "computer_list_windows", "browser_extract", "browser_state"] {
            assert!(!invalidates_native_observation(tool), "{tool}");
        }
    }
}

/// A read-only detail view in ORIGINAL image pixels. No source file is changed.
#[derive(Debug, Clone, Copy, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImageRegion {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

pub(crate) fn image_source_dimensions(path: &Path) -> Result<(u32, u32)> {
    let bytes = read_vision_image_bounded(path)?;
    let dimensions = match image::guess_format(&bytes)? {
        image::ImageFormat::Gif => gif_dimensions(&bytes)?,
        image::ImageFormat::WebP => webp_dimensions(&bytes)?,
        image::ImageFormat::Png | image::ImageFormat::Jpeg => {
            image::ImageReader::new(std::io::Cursor::new(bytes))
                .with_guessed_format()?
                .into_dimensions()?
        }
        _ => anyhow::bail!("Unsupported image format"),
    };
    ensure_vision_dimensions(dimensions.0, dimensions.1)?;
    Ok(dimensions)
}

pub(crate) async fn image_region_data_uri(path: &Path, region: ImageRegion) -> Result<String> {
    let path = path.to_path_buf();
    let task = tokio::task::spawn_blocking(move || {
        let bytes = read_vision_image_bounded(&path)?;
        build_region_data_uri(&bytes, region)
    });
    tokio::time::timeout(VISION_PAYLOAD_TIMEOUT, task)
        .await
        .context("vision: detail preparation timed out")?
        .context("vision: detail task failed")?
}

fn build_region_data_uri(bytes: &[u8], region: ImageRegion) -> Result<String> {
    anyhow::ensure!(
        bytes.len() as u64 <= VISION_INPUT_MAX_BYTES,
        "vision image exceeds its byte limit"
    );
    anyhow::ensure!(
        region.width > 0 && region.height > 0,
        "Detail width and height must be positive"
    );
    let format = image::guess_format(bytes).context("vision image format was not recognized")?;
    anyhow::ensure!(
        matches!(format, image::ImageFormat::Png | image::ImageFormat::Jpeg),
        "Detail crops currently require PNG or JPEG; export a still image first"
    );
    let mut reader = image::ImageReader::with_format(std::io::Cursor::new(bytes), format);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(VISION_DECODE_MAX_EDGE);
    limits.max_image_height = Some(VISION_DECODE_MAX_EDGE);
    limits.max_alloc = Some(VISION_DECODE_MAX_ALLOC);
    reader.limits(limits);
    let decoded = reader
        .decode()
        .context("vision image could not be decoded within the size limits")?;
    let (width, height) = (decoded.width(), decoded.height());
    anyhow::ensure!(region.x.checked_add(region.width).is_some_and(|end|end<=width)
        && region.y.checked_add(region.height).is_some_and(|end|end<=height),
        "Detail region is outside the original {width}x{height} image; use source-image pixel coordinates");
    let crop = decoded.crop_imm(region.x, region.y, region.width, region.height);
    let crop = if crop.width().max(crop.height()) > VISION_MAX_EDGE {
        crop.resize(
            VISION_MAX_EDGE,
            VISION_MAX_EDGE,
            image::imageops::FilterType::Triangle,
        )
    } else {
        crop
    };
    let mut encoded = std::io::Cursor::new(Vec::new());
    crop.write_to(&mut encoded, image::ImageFormat::Png)?;
    Ok(format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(encoded.into_inner())
    ))
}

pub(crate) async fn analyze_image_region(
    config: &VisionConfig,
    path: &Path,
    question: Option<&str>,
    region: ImageRegion,
) -> Result<String> {
    let data = image_region_data_uri(path, region).await?;
    let prompt=format!("Inspect this detail crop from source-image pixels x={}, y={}, width={}, height={}. It is only a detail view, not proof that the full artifact is complete. Describe visible defects and uncertainty. Do not infer anything outside the crop. Question: {}",region.x,region.y,region.width,region.height,question.unwrap_or("Inspect visible detail and defects."));
    ask_about_image(config, &data, &prompt).await
}

/// Acting models issue coordinates directly in the captured screen's pixels.
/// Unlike a grounding sidecar, no later stage rescales their tool arguments.
/// Preserve the pixel grid while retaining bounded decoding and JPEG encoding.
pub async fn native_screen_data_uri(png_path: &Path) -> Result<String> {
    Ok(prepare_vision_payload(png_path, VISION_DECODE_MAX_EDGE).await?.data_uri)
}

/// A validated full-document design capture can be taller than a desktop.
/// Preserve its original pixels for reference comparison without relaxing the
/// ordinary desktop/reference decoder. The design capture validator already
/// limits PNG height to 12,000 and inflated image bytes to 200 MB; repeat those
/// boundaries here before the image reaches the provider request.
pub(crate) async fn native_design_capture_data_uri(png_path: &Path) -> Result<String> {
    let path = png_path.to_path_buf();
    let task = tokio::task::spawn_blocking(move || {
        let bytes = read_vision_image_bounded(&path)?;
        anyhow::ensure!(image::guess_format(&bytes)? == image::ImageFormat::Png,
            "a managed design capture must be a PNG");
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(VISION_DECODE_MAX_EDGE);
        limits.max_image_height = Some(12_000);
        limits.max_alloc = Some(200_000_000);
        let (jpeg, scale) = encode_to_jpeg_with_limits(&bytes, 12_000, limits)
            .context("managed design PNG exceeds its full-document decoding limits or is corrupt")?;
        anyhow::ensure!(scale == 1.0, "managed design capture unexpectedly changed scale");
        let data = format!("data:image/jpeg;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(jpeg));
        anyhow::ensure!(data.len() <= VISION_DATA_URI_MAX_BYTES,
            "managed design capture exceeds its encoded image limit");
        Ok::<_, anyhow::Error>(data)
    });
    tokio::time::timeout(VISION_PAYLOAD_TIMEOUT, task).await
        .context("managed design image preparation timed out")?
        .context("managed design image preparation task failed")?
}

const OCR_PROMPT: &str = "Transcribe ALL visible text in this screenshot VERBATIM (the Coasty `ocr` task). Preserve reading order and group by region — window title bar, menus/toolbar, main content, any dialog or popup, status bar. Output only the literal text you can read, one logical line per UI line; do NOT summarize, describe, translate, or add commentary. Skip regions with no text. If the screen has no readable text, output exactly: (no text).";

/// Verbatim screen-text transcription (Coasty's `ocr`). Use over
/// `describe_screenshot` when the agent needs EXACT strings — error-dialog
/// wording, table cell values, codes, file names — not a description.
pub async fn extract_screen_text(config: &VisionConfig, png_path: &Path) -> Result<String> {
    ask_about_screenshot(config, png_path, OCR_PROMPT).await
}

/// UI-TARS-style grounding (donor: ByteDance UI-TARS prompt format): ask the
/// vision model WHERE a described element is, in pixel coordinates of the
/// screenshot. Works with any capable VLM today; pointing `vision_model` at a
/// real UI-TARS endpoint upgrades precision with zero code changes.
///
/// The model sees the (possibly downscaled) vision payload; the returned point
/// is mapped back to ORIGINAL screenshot pixels, so callers can click it
/// directly.
pub async fn locate_target(
    config: &VisionConfig,
    png_path: &Path,
    description: &str,
) -> Result<(i64, i64)> {
    let prompt = format!(
        "You are a GUI grounding agent. In this screenshot, locate: {description}\n\nOutput EXACTLY one line in this format and nothing else:\nclick(point='<point>x y</point>')\nwhere x and y are pixel coordinates in this image (origin top-left). If the element is not visible, output exactly: not_found"
    );
    let payload = vision_payload(png_path).await?;
    let answer = ask_about_image(config, &payload.data_uri, &prompt).await?;
    if answer.to_lowercase().contains("not_found") {
        anyhow::bail!("grounding: target not visible on the current screenshot");
    }
    let (x, y) = parse_point(&answer)
        .with_context(|| format!("grounding: unparseable model output: {answer}"))?;
    Ok((
        (x as f64 * payload.scale).round() as i64,
        (y as f64 * payload.scale).round() as i64,
    ))
}

/// First `x y` / `x, y` pair in UI-TARS output shapes:
/// `<point>197 525</point>`, `(197,525)`, `click(start_box='(197,525)')`.
pub fn parse_point(text: &str) -> Option<(i64, i64)> {
    let mut digits: Vec<i64> = Vec::new();
    let mut current = String::new();
    for ch in text.chars() {
        if ch.is_ascii_digit() {
            current.push(ch);
        } else {
            if !current.is_empty() {
                if let Ok(value) = current.parse() {
                    digits.push(value);
                }
                current.clear();
            }
        }
        if digits.len() == 2 {
            return Some((digits[0], digits[1]));
        }
    }
    if !current.is_empty() && digits.len() == 1 {
        if let Ok(value) = current.parse() {
            return Some((digits[0], value));
        }
    }
    None
}

/// The image actually shipped to the vision model, plus the factor that maps
/// its pixel space back to the ORIGINAL screenshot's pixels (>= 1.0).
struct VisionPayload {
    data_uri: String,
    scale: f64,
}

/// Long-edge cap for sidecar and artifact payloads. Sidecar grounding maps
/// its coordinates back afterward. Native desktop attachments preserve their
/// coordinate grid instead; focused window capture keeps those payloads small.
const VISION_MAX_EDGE: u32 = 1440;
const VISION_JPEG_QUALITY: u8 = 80;
const VISION_INPUT_MAX_BYTES: u64 = 32 * 1024 * 1024;
const VISION_DECODE_MAX_EDGE: u32 = 8_192;
const VISION_DECODE_MAX_ALLOC: u64 = 128 * 1024 * 1024;
const VISION_DATA_URI_MAX_BYTES: usize = 46 * 1024 * 1024;
const VISION_MAX_PIXELS: u64 = 64 * 1024 * 1024;
const VISION_PAYLOAD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

fn read_vision_image_bounded(path: &Path) -> Result<Vec<u8>> {
    use std::io::Read;

    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let mut file = options
        .open(path)
        .with_context(|| format!("vision: failed to open image {}", path.display()))?;
    let metadata = file
        .metadata()
        .with_context(|| format!("vision: failed to inspect image {}", path.display()))?;
    anyhow::ensure!(
        metadata.is_file(),
        "vision: {} is not a regular image file",
        path.display()
    );
    anyhow::ensure!(
        metadata.len() <= VISION_INPUT_MAX_BYTES,
        "vision: image {} is {} bytes; maximum is {VISION_INPUT_MAX_BYTES}",
        path.display(),
        metadata.len()
    );
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.by_ref()
        .take(VISION_INPUT_MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("vision: failed to read image {}", path.display()))?;
    anyhow::ensure!(
        bytes.len() as u64 <= VISION_INPUT_MAX_BYTES,
        "vision: image {} grew beyond the {VISION_INPUT_MAX_BYTES}-byte limit",
        path.display()
    );
    Ok(bytes)
}

/// Build the vision payload for a screenshot: decode, downscale to the cap,
/// re-encode as JPEG (a fraction of the PNG's bytes). Runs on the blocking
/// pool — a full-desktop decode+resize is real CPU work. PNG/JPEG decode
/// failures are errors; supported GIF/WebP files pass through only after a
/// bounded header/dimension check.
async fn vision_payload(png_path: &Path) -> Result<VisionPayload> {
    prepare_vision_payload(png_path, VISION_MAX_EDGE).await
}

async fn prepare_vision_payload(png_path: &Path, max_edge: u32) -> Result<VisionPayload> {
    let path = png_path.to_path_buf();
    let task = tokio::task::spawn_blocking(move || {
        let bytes = read_vision_image_bounded(&path)?;
        build_vision_payload_for_edge(&bytes, max_edge)
    });
    let payload = tokio::time::timeout(VISION_PAYLOAD_TIMEOUT, task)
        .await
        .context("vision: payload preparation timed out after 30 seconds")?
        .context("vision: payload task failed")??;
    Ok(payload)
}

fn ensure_vision_dimensions(width: u32, height: u32) -> Result<()> {
    anyhow::ensure!(width > 0 && height > 0, "vision image had zero dimensions");
    anyhow::ensure!(
        width <= VISION_DECODE_MAX_EDGE && height <= VISION_DECODE_MAX_EDGE,
        "vision image dimensions {width}x{height} exceed the {VISION_DECODE_MAX_EDGE}-pixel edge limit"
    );
    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .context("vision image dimensions overflowed")?;
    anyhow::ensure!(
        pixels <= VISION_MAX_PIXELS,
        "vision image has {pixels} pixels; maximum is {VISION_MAX_PIXELS}"
    );
    Ok(())
}

fn gif_dimensions(bytes: &[u8]) -> Result<(u32, u32)> {
    let image_descriptor = bytes
        .get(10..)
        .and_then(|rest| rest.iter().position(|b| *b == 0x2c));
    let trailer = bytes
        .get(10..)
        .and_then(|rest| rest.iter().rposition(|b| *b == 0x3b));
    anyhow::ensure!(
        bytes.len() >= 14
            && (&bytes[..6] == b"GIF87a" || &bytes[..6] == b"GIF89a")
            && image_descriptor.is_some()
            && trailer.is_some()
            && image_descriptor < trailer,
        "vision GIF header was invalid"
    );
    Ok((
        u32::from(u16::from_le_bytes([bytes[6], bytes[7]])),
        u32::from(u16::from_le_bytes([bytes[8], bytes[9]])),
    ))
}

fn webp_has_image_chunk(bytes: &[u8], declared: usize) -> bool {
    let mut offset = 12usize;
    while offset.checked_add(8).is_some_and(|end| end <= declared) {
        let kind = &bytes[offset..offset + 4];
        let length = u32::from_le_bytes([
            bytes[offset + 4],
            bytes[offset + 5],
            bytes[offset + 6],
            bytes[offset + 7],
        ]) as usize;
        let Some(data_end) = offset
            .checked_add(8)
            .and_then(|start| start.checked_add(length))
        else {
            return false;
        };
        if data_end > declared || data_end > bytes.len() {
            return false;
        }
        if matches!(kind, b"VP8 " | b"VP8L") {
            return true;
        }
        let Some(next) = data_end.checked_add(length & 1) else {
            return false;
        };
        offset = next;
    }
    false
}

fn webp_dimensions(bytes: &[u8]) -> Result<(u32, u32)> {
    anyhow::ensure!(
        bytes.len() >= 20 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP",
        "vision WebP header was invalid"
    );
    let declared = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as u64 + 8;
    anyhow::ensure!(
        declared >= 20 && declared <= bytes.len() as u64,
        "vision WebP RIFF length was invalid"
    );
    anyhow::ensure!(
        webp_has_image_chunk(bytes, declared as usize),
        "vision WebP contained no bounded image chunk"
    );
    let chunk_len = u32::from_le_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]) as u64;
    anyhow::ensure!(
        chunk_len <= bytes.len().saturating_sub(20) as u64,
        "vision WebP chunk length exceeded the file"
    );
    match &bytes[12..16] {
        b"VP8X" => {
            anyhow::ensure!(
                chunk_len >= 10 && bytes.len() >= 30,
                "vision extended WebP header was truncated"
            );
            let width = 1
                + u32::from(bytes[24])
                + (u32::from(bytes[25]) << 8)
                + (u32::from(bytes[26]) << 16);
            let height = 1
                + u32::from(bytes[27])
                + (u32::from(bytes[28]) << 8)
                + (u32::from(bytes[29]) << 16);
            Ok((width, height))
        }
        b"VP8 " => {
            anyhow::ensure!(
                chunk_len >= 10 && bytes.len() >= 30 && &bytes[23..26] == b"\x9d\x01\x2a",
                "vision lossy WebP frame header was invalid"
            );
            let width = u32::from(u16::from_le_bytes([bytes[26], bytes[27]]) & 0x3fff);
            let height = u32::from(u16::from_le_bytes([bytes[28], bytes[29]]) & 0x3fff);
            Ok((width, height))
        }
        b"VP8L" => {
            anyhow::ensure!(
                chunk_len >= 5 && bytes.len() >= 25,
                "vision lossless WebP header was truncated"
            );
            anyhow::ensure!(
                bytes[20] == 0x2f,
                "vision lossless WebP signature was invalid"
            );
            let width = 1 + u32::from(bytes[21]) + (u32::from(bytes[22] & 0x3f) << 8);
            let height = 1
                + u32::from(bytes[22] >> 6)
                + (u32::from(bytes[23]) << 2)
                + (u32::from(bytes[24] & 0x0f) << 10);
            Ok((width, height))
        }
        other => anyhow::bail!(
            "vision WebP chunk {:?} is unsupported",
            String::from_utf8_lossy(other)
        ),
    }
}

#[cfg(test)]
fn build_vision_payload(bytes: &[u8]) -> Result<VisionPayload> {
    build_vision_payload_for_edge(bytes, VISION_MAX_EDGE)
}

fn build_vision_payload_for_edge(bytes: &[u8], max_edge: u32) -> Result<VisionPayload> {
    anyhow::ensure!(
        bytes.len() as u64 <= VISION_INPUT_MAX_BYTES,
        "vision image exceeds the {VISION_INPUT_MAX_BYTES}-byte limit"
    );
    let format = image::guess_format(bytes).context("vision image format was not recognized")?;
    match format {
        image::ImageFormat::Png | image::ImageFormat::Jpeg => {
            let (jpeg, scale) = encode_to_jpeg(bytes, max_edge).context(
                "vision PNG/JPEG could not be decoded within the strict dimension/allocation limits",
            )?;
            Ok(VisionPayload {
                data_uri: format!(
                    "data:image/jpeg;base64,{}",
                    base64::engine::general_purpose::STANDARD.encode(jpeg)
                ),
                scale,
            })
        }
        image::ImageFormat::Gif => {
            let (width, height) = gif_dimensions(bytes)?;
            ensure_vision_dimensions(width, height)?;
            Ok(VisionPayload {
                data_uri: format!(
                    "data:image/gif;base64,{}",
                    base64::engine::general_purpose::STANDARD.encode(bytes)
                ),
                scale: 1.0,
            })
        }
        image::ImageFormat::WebP => {
            let (width, height) = webp_dimensions(bytes)?;
            ensure_vision_dimensions(width, height)?;
            Ok(VisionPayload {
                data_uri: format!(
                    "data:image/webp;base64,{}",
                    base64::engine::general_purpose::STANDARD.encode(bytes)
                ),
                scale: 1.0,
            })
        }
        other => anyhow::bail!(
            "vision image format {other:?} is not supported; use PNG, JPEG, GIF, or WebP"
        ),
    }
}

/// Decode → bound the long edge → JPEG. Returns None when the image can't be
/// decoded within the strict limits. The scale is
/// original_width / encoded_width, so `encoded_point * scale = screen_point`.
#[cfg(test)]
fn downscale_to_jpeg(png: &[u8]) -> Option<(Vec<u8>, f64)> {
    encode_to_jpeg(png, VISION_MAX_EDGE)
}

fn encode_to_jpeg(png: &[u8], max_edge: u32) -> Option<(Vec<u8>, f64)> {
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(VISION_DECODE_MAX_EDGE);
    limits.max_image_height = Some(VISION_DECODE_MAX_EDGE);
    limits.max_alloc = Some(VISION_DECODE_MAX_ALLOC);
    encode_to_jpeg_with_limits(png, max_edge, limits)
}

fn encode_to_jpeg_with_limits(png: &[u8], max_edge: u32, limits: image::Limits) -> Option<(Vec<u8>, f64)> {
    let mut reader = image::ImageReader::new(std::io::Cursor::new(png))
        .with_guessed_format()
        .ok()?;
    reader.limits(limits);
    let decoded = reader.decode().ok()?;
    if u64::from(decoded.width()).checked_mul(u64::from(decoded.height()))? > VISION_MAX_PIXELS {
        return None;
    }
    let original_width = decoded.width();
    let resized = if decoded.width().max(decoded.height()) > max_edge {
        decoded.resize(
            max_edge,
            max_edge,
            image::imageops::FilterType::Triangle,
        )
    } else {
        decoded
    };
    let scale = original_width as f64 / resized.width() as f64;
    let mut out = Vec::new();
    // JPEG has no alpha — flatten to RGB before encoding.
    let rgb = resized.to_rgb8();
    let mut encoder =
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, VISION_JPEG_QUALITY);
    encoder.encode_image(&rgb).ok()?;
    Some((out, scale))
}

async fn ask_about_screenshot(
    config: &VisionConfig,
    png_path: &Path,
    prompt: &str,
) -> Result<String> {
    let payload = vision_payload(png_path).await?;
    ask_about_image(config, &payload.data_uri, prompt).await
}

/// Ask the vision model anything about a supported web image file — the `image_analyze`
/// tool's engine (user ask 2026-07-10: Pixel was opening image files in a
/// desktop viewer and screenshotting the screen to "see" them; this reads the
/// file directly). No question = a thorough description.
pub async fn analyze_image_file(
    config: &VisionConfig,
    image_path: &Path,
    question: Option<&str>,
) -> Result<String> {
    let prompt = match question {
        Some(q) if !q.trim().is_empty() => q.trim().to_string(),
        _ => "Describe this image thoroughly: what it shows, all visible text verbatim, \
              layout/structure, notable details, and anything unusual."
            .to_string(),
    };
    let grounded_prompt = format!(
        "Inspect the image, not the desired outcome described in the question. Treat requested qualities as hypotheses. Report visible contradictions and uncertainty; do not rubber-stamp a request to confirm success. For framing checks, inspect all four image edges for cropped subject geometry. Distinguish visible evidence from specifications that pixels cannot prove.\n\nQuestion: {prompt}"
    );
    ask_about_screenshot(config, image_path, &grounded_prompt).await
}

/// Run a vision/text turn through the SuperGrok CLI proxy provider (streaming
/// + auth headers handled there), returning the assembled text. Shared by the
/// text-extract and image paths above.
async fn grok_cli_complete(
    config: &VisionConfig,
    messages: Vec<crate::providers::ChatMessage>,
) -> Result<String> {
    let token = config
        .api_key
        .clone()
        .context("vision via grok-cli needs the OAuth access token")?;
    use crate::providers::LLMProvider;
    let provider = crate::providers::grok_cli::GrokCliProvider::with_url_and_timeout(
        config.base_url.clone(),
        token,
        std::time::Duration::from_secs(90),
    );
    let request = crate::providers::CompletionRequest::new(&config.model, messages);
    let response = provider.complete(request).await?;
    let text = response.content.trim().to_string();
    anyhow::ensure!(!text.is_empty(), "grok-cli vision returned no text");
    Ok(text)
}

/// Text-only sidecar (page-text extraction) through the grok-cli proxy.
async fn grok_cli_sidecar(
    config: &VisionConfig,
    instructions: &str,
    prompt: &str,
) -> Result<String> {
    let messages = vec![
        crate::providers::ChatMessage::system(instructions),
        crate::providers::ChatMessage::user(prompt),
    ];
    grok_cli_complete(config, messages).await
}

/// OpenAI-compatible chat URL for the generic sidecar paths. Every catalog
/// provider carries its API path in base_url (".../v1") EXCEPT ollama-cloud,
/// whose base_url is the bare site root (the native API lives at /api/*):
/// posting to {root}/chat/completions hits the website and returns 404 HTML.
/// Its OpenAI-compat surface is under /v1.
fn chat_completions_url(config: &VisionConfig) -> String {
    let base = config.base_url.trim_end_matches('/');
    if config.provider_id == "ollama-cloud" && !base.ends_with("/v1") {
        format!("{base}/v1/chat/completions")
    } else {
        format!("{base}/chat/completions")
    }
}

async fn ask_about_image(config: &VisionConfig, data_uri: &str, prompt: &str) -> Result<String> {
    anyhow::ensure!(
        data_uri.len() <= VISION_DATA_URI_MAX_BYTES,
        "vision image payload exceeds the {VISION_DATA_URI_MAX_BYTES}-byte limit"
    );
    anyhow::ensure!(
        prompt.chars().take(VISION_QUESTION_MAX_CHARS + 1).count() <= VISION_QUESTION_MAX_CHARS,
        "vision prompt exceeds the {VISION_QUESTION_MAX_CHARS}-character limit"
    );
    let mut errors = Vec::new();
    for candidate in std::iter::once(config).chain(config.fallbacks.iter()) {
        match ask_about_image_once(candidate, data_uri, prompt).await {
            Ok(answer) => return Ok(answer),
            Err(error) => errors.push(format!(
                "{} ({}): {error:#}",
                candidate.provider_id, candidate.model
            )),
        }
    }
    anyhow::bail!("vision chain exhausted: {}", errors.join(" | "))
}

async fn ask_about_image_once(
    config: &VisionConfig,
    data_uri: &str,
    prompt: &str,
) -> Result<String> {
    if config.provider_id == "openai-codex" {
        let token = config
            .api_key
            .clone()
            .context("vision via openai-codex needs the OAuth access token")?;
        let provider = crate::providers::openai_codex::OpenAICodexProvider::with_url_and_timeout(
            config.base_url.clone(),
            token,
            std::time::Duration::from_secs(60),
        );
        return provider
            .complete_vision(&config.model, prompt, &data_uri)
            .await;
    }
    // SuperGrok CLI proxy: grok-4.5 has vision, but the endpoint is
    // streaming-only and header-gated. Send the image through the provider.
    if config.provider_id == "grok-cli" {
        let msg =
            crate::providers::ChatMessage::user_with_images(prompt, vec![data_uri.to_string()]);
        return grok_cli_complete(config, vec![msg]).await;
    }
    let body = json!({
        "model": config.model,
        "max_tokens": 500,
        "messages": [{
            "role": "user",
            "content": [
                {"type": "text", "text": prompt},
                {"type": "image_url", "image_url": {"url": data_uri}}
            ]
        }]
    });
    let url = chat_completions_url(config);
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(60))
        .build()?;
    let mut request = client.post(&url).json(&body);
    if let Some(key) = &config.api_key {
        request = request.bearer_auth(key);
    }
    let response = request.send().await.context("vision request failed")?;
    let payload = read_vision_json_response(response, "vision response")
        .await
        .with_context(|| format!("vision call to {url} failed"))?;
    payload
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|t| t.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .context("vision response had no message content")
}
