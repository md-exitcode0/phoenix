//! Image generation — the creating-images twin of the vision sidecar
//! (which reads them). 2026-07-07 goal: a first-class capability, aimed at
//! frontend/design work (Iris): hero images, textures, illustration and
//! mood references that lift a UI out of the css-gradient uncanny valley.
//! The capability is universal: every named coworker receives the same tool;
//! role knowledge decides who usually owns a design, not who may create one.
//!
//! Provider contract: the configured `image_provider` must speak the
//! OpenAI-compatible `POST /images/generations` API (`gpt-image-1`,
//! Together/Fireworks/Novita FLUX endpoints, xAI grok-2-image, …). The
//! model comes from `image_model` in `[profile.<name>.llm]`; both are
//! picked during onboarding. Unset = the tool reports itself disabled.
//!
//! Files land under `<workspace>/artifacts/images/` — workspace-relative so
//! the frontend agent can reference them from HTML/CSS immediately, named
//! from the prompt so a directory of generations stays navigable.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use base64::Engine;
use serde::Deserialize;
use serde_json::json;

use crate::config::auth_profile::resolve_llm_auth;
use crate::config::LLMProfile;
use crate::providers::providers_data;

const IMAGE_PROMPT_MAX_CHARS: usize = 32_768;
const IMAGE_SIZE_MAX_CHARS: usize = 64;
const IMAGE_URL_MAX_CHARS: usize = 8_192;
const IMAGE_MAX_BYTES: usize = 32 * 1024 * 1024;
// 32 MiB of image bytes expands to just under 43 MiB of base64. Leave room
// for the surrounding JSON envelope without allowing an unbounded response.
const IMAGE_JSON_MAX_BYTES: usize = 46 * 1024 * 1024;
const IMAGE_ERROR_BODY_MAX_BYTES: usize = 256 * 1024;
const IMAGE_ERROR_PREVIEW_CHARS: usize = 500;
const IMAGE_MAX_EDGE: u32 = 8_192;
const IMAGE_MAX_PIXELS: u64 = 64 * 1024 * 1024;
const IMAGE_CONFIG_HELP: &str = "image generation is not configured. Open Settings → Models & Providers → Image generation, choose a primary provider/model, and optionally order one or more image fallbacks. Phoenix will not pretend an ordinary chat or vision model can generate images.";

#[derive(Debug, Deserialize)]
pub struct ImageGenInput {
    /// What to generate. Specific beats vague: subject, style, palette,
    /// lighting, composition.
    pub prompt: String,
    /// Optional file stem for the output (slugified). Default: from the prompt.
    #[serde(default)]
    pub name: Option<String>,
    /// Optional size hint, e.g. "1024x1024" (default), "1536x1024", "1024x1536".
    #[serde(default)]
    pub size: Option<String>,
}

/// Resolved endpoint + credential, mirroring `vision::VisionConfig`.
#[derive(Debug, Clone)]
pub struct ImageGenConfig {
    pub provider_id: String,
    pub model: String,
    pub base_url: String,
    pub api_key: Option<String>,
    pub fallbacks: Vec<ImageGenConfig>,
}

/// Build the image-gen config from the active LLM profile. None = disabled.
pub fn image_gen_config(llm: &LLMProfile) -> Result<Option<ImageGenConfig>> {
    let Some(model) = llm.image_model.clone().filter(|m| !m.trim().is_empty()) else {
        return Ok(None);
    };
    let provider_id = llm
        .image_provider
        .clone()
        .filter(|p| !p.trim().is_empty())
        .unwrap_or_else(|| llm.provider.clone());
    let provider = providers_data::get_provider(&provider_id).with_context(|| {
        format!("image_provider `{provider_id}` is not in the provider catalog")
    })?;
    // Cross-provider probe (same fix as the vision lane, 2026-07-09): the
    // main lane's pinned auth must not ride into another provider's lookup.
    let probe = llm
        .probe_for_provider(&provider_id)
        .with_lane_pin("image", &provider_id);
    match resolve_llm_auth(&probe, &provider, |name| std::env::var(name).ok()) {
        Ok(auth) => {
            let fallbacks = image_chain_configs(llm, &model, &provider_id)
                .context("could not load the image fallback credential store")?;
            Ok(Some(ImageGenConfig {
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
            // Account fallback (plan 018): the image chain keeps the tool
            // alive when the primary credential dies.
            let mut chain = image_chain_configs(llm, &model, &provider_id)
                .context("could not load the image fallback credential store")?;
            if !chain.is_empty() {
                let mut config = chain.remove(0);
                config.fallbacks = chain;
                eprintln!(
                    "warning: image-gen primary auth failed ({primary_error:#}) — using fallback profile lane {} ({})",
                    config.provider_id, config.model
                );
                return Ok(Some(config));
            }
            Err(primary_error).with_context(|| {
                format!("could not resolve auth for image provider `{provider_id}`")
            })
        }
    }
}

/// First usable account in the image fallback chain — mirrors the vision
/// lane: same provider keeps the configured model, a different provider
/// needs an explicit "image" assignment or is skipped.
fn image_chain_configs(
    llm: &LLMProfile,
    configured_model: &str,
    configured_provider: &str,
) -> Result<Vec<ImageGenConfig>> {
    if llm.fallback.image.is_empty() {
        return Ok(vec![]);
    }
    let store = crate::config::auth_profile::load_auth_profile_store()?;
    let mut configs = Vec::new();
    for profile_id in &llm.fallback.image {
        let Some(credential) = store.profiles.get(profile_id) else {
            continue;
        };
        let provider_id = crate::config::auth_profile::profile_provider_id(credential).to_string();
        let Some(provider) = providers_data::get_provider(&provider_id) else {
            continue;
        };
        let model = match store.assignment_for(profile_id, "image") {
            Some(assignment) => assignment.model,
            None if provider_id == configured_provider => configured_model.to_string(),
            None => continue,
        };
        let Ok(secret) = crate::config::auth_profile::extract_profile_secret(credential) else {
            continue;
        };
        configs.push(ImageGenConfig {
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

/// Execute one generation: call the API, write the PNG under
/// `<workspace>/artifacts/images/`, return the workspace-relative path plus
/// usage guidance. All errors are honest tool errors — never a panic.
pub async fn execute(workspace_root: &Path, input: ImageGenInput) -> Result<String> {
    let prompt = input.prompt.trim();
    if prompt.is_empty() {
        anyhow::bail!("image_gen needs a `prompt` — what should the image show?");
    }
    let llm = crate::config::PhoenixConfig::load()
        .context("could not load config")?
        .profile
        .llm;
    let Some(config) = image_gen_config(&llm)? else {
        anyhow::bail!(IMAGE_CONFIG_HELP);
    };
    let bytes = generate(&config, prompt, input.size.as_deref()).await?;
    let extension = validate_generated_image(&bytes)?;
    let dir = workspace_root.join("artifacts/images");
    std::fs::create_dir_all(&dir).with_context(|| format!("could not create {}", dir.display()))?;
    let stem = slug(input.name.as_deref().unwrap_or(prompt));
    let (path, mut file) = create_unique_image(&dir, &stem, extension)?;
    let persist_result = file
        .write_all(&bytes)
        .with_context(|| format!("could not write {}", path.display()))
        .and_then(|()| {
            file.sync_all()
                .with_context(|| format!("could not sync {}", path.display()))
        });
    if let Err(error) = persist_result {
        drop(file);
        let _ = std::fs::remove_file(&path);
        return Err(error);
    }
    let rel = path
        .strip_prefix(workspace_root)
        .unwrap_or(&path)
        .display()
        .to_string();
    Ok(format!(
        "Image generated: {rel} ({} KB, model {}). Reference it from HTML/CSS by that \
         relative path; regenerate with a refined prompt rather than editing pixels.",
        bytes.len() / 1024,
        config.model,
    ))
}

/// The raw API call, separated for testing and reuse. Accepts both response
/// shapes: `data[0].b64_json` (OpenAI default for gpt-image-1) and
/// `data[0].url` (hosted outputs — fetched with the same client).
pub async fn generate(
    config: &ImageGenConfig,
    prompt: &str,
    size: Option<&str>,
) -> Result<Vec<u8>> {
    let mut errors = Vec::new();
    for candidate in std::iter::once(config).chain(config.fallbacks.iter()) {
        match generate_once(candidate, prompt, size).await {
            Ok(bytes) => return Ok(bytes),
            Err(error) => errors.push(format!(
                "{} ({}): {error:#}",
                candidate.provider_id, candidate.model
            )),
        }
    }
    anyhow::bail!("image generation chain exhausted: {}", errors.join(" | "))
}

async fn generate_once(
    config: &ImageGenConfig,
    prompt: &str,
    size: Option<&str>,
) -> Result<Vec<u8>> {
    anyhow::ensure!(
        !config.model.trim().is_empty() && config.model.chars().count() <= 4_096,
        "image generation model id is empty or too long"
    );
    anyhow::ensure!(
        !config.base_url.trim().is_empty()
            && config
                .base_url
                .chars()
                .take(IMAGE_URL_MAX_CHARS + 1)
                .count()
                <= IMAGE_URL_MAX_CHARS,
        "image generation base URL is empty or too long"
    );
    let prompt = prompt.trim();
    anyhow::ensure!(
        !prompt.is_empty(),
        "image generation prompt cannot be empty"
    );
    anyhow::ensure!(
        prompt.chars().take(IMAGE_PROMPT_MAX_CHARS + 1).count() <= IMAGE_PROMPT_MAX_CHARS,
        "image generation prompt exceeds the {IMAGE_PROMPT_MAX_CHARS}-character limit"
    );
    let mut body = json!({
        "model": config.model,
        "prompt": prompt,
        "n": 1,
    });
    if let Some(size) = size.map(str::trim).filter(|s| !s.is_empty()) {
        anyhow::ensure!(
            size.chars().take(IMAGE_SIZE_MAX_CHARS + 1).count() <= IMAGE_SIZE_MAX_CHARS
                && !size.chars().any(char::is_control),
            "image generation size is invalid or exceeds {IMAGE_SIZE_MAX_CHARS} characters"
        );
        body["size"] = json!(size);
    }
    let url = format!(
        "{}/images/generations",
        config.base_url.trim_end_matches('/')
    );
    let client = reqwest::Client::builder()
        // Image generation is slow — diffusion models regularly take >60s.
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(180))
        .build()?;
    let mut request = client.post(&url).json(&body);
    if let Some(key) = &config.api_key {
        request = request.bearer_auth(key);
    }
    let response = request
        .send()
        .await
        .context("image generation request failed")?;
    let status = response.status();
    let response_limit = if status.is_success() {
        IMAGE_JSON_MAX_BYTES
    } else {
        IMAGE_ERROR_BODY_MAX_BYTES
    };
    let raw =
        crate::providers::read_response_text(response, response_limit, "image generation response")
            .await
            .with_context(|| format!("failed to read image generation response (HTTP {status})"))?;
    let payload: serde_json::Value = serde_json::from_str(&raw).with_context(|| {
        format!(
            "image generation response was not valid JSON (HTTP {status}): {}",
            image_error_preview(&raw)
        )
    })?;
    if status.is_success() && payload.get("error").is_some_and(|error| !error.is_null()) {
        let detail = payload
            .pointer("/error/message")
            .or_else(|| payload.get("message"))
            .and_then(|message| message.as_str())
            .unwrap_or(&raw);
        anyhow::bail!(
            "image model reported an error despite HTTP {status}: {}",
            image_error_preview(detail)
        );
    }
    if !status.is_success() {
        let detail = payload
            .get("error")
            .and_then(|e| e.get("message"))
            .and_then(|m| m.as_str())
            .or_else(|| payload.get("message").and_then(|m| m.as_str()))
            .unwrap_or(&raw);
        anyhow::bail!(
            "image model returned {status}: {}",
            image_error_preview(detail)
        );
    }
    let first = payload
        .get("data")
        .and_then(|d| d.get(0))
        .context("image response had no data[0]")?;
    if let Some(b64) = first.get("b64_json").and_then(|v| v.as_str()) {
        let max_base64_chars = ((IMAGE_MAX_BYTES + 2) / 3) * 4;
        anyhow::ensure!(
            b64.len() <= max_base64_chars,
            "image b64_json exceeds the {IMAGE_MAX_BYTES}-byte decoded limit"
        );
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(b64)
            .context("image b64_json did not decode")?;
        anyhow::ensure!(
            bytes.len() <= IMAGE_MAX_BYTES,
            "decoded image exceeds the {IMAGE_MAX_BYTES}-byte limit"
        );
        validate_generated_image(&bytes)?;
        return Ok(bytes);
    }
    if let Some(url) = first.get("url").and_then(|v| v.as_str()) {
        validate_generated_image_url(url)?;
        let response = client.get(url).send().await.map_err(|error| {
            anyhow::anyhow!(
                "fetching generated image URL failed: {}",
                error.without_url()
            )
        })?;
        validate_generated_image_url(response.url().as_str())
            .context("generated image redirect target was invalid")?;
        let image_status = response.status();
        if !image_status.is_success() {
            let detail = match crate::providers::read_response_text(
                response,
                IMAGE_ERROR_BODY_MAX_BYTES,
                "generated image download error",
            )
            .await
            {
                Ok(body) => image_error_preview(&body),
                Err(error) => format!("response body unavailable: {error}"),
            };
            anyhow::bail!("generated image download returned {image_status}: {detail}");
        }
        let bytes = crate::providers::read_response_bytes(
            response,
            IMAGE_MAX_BYTES,
            "generated image download",
        )
        .await
        .with_context(|| format!("reading generated image bytes failed (HTTP {image_status})"))?;
        validate_generated_image(&bytes)?;
        return Ok(bytes);
    }
    anyhow::bail!("image response had neither b64_json nor url in data[0]");
}

fn image_error_preview(value: &str) -> String {
    let preview: String = value
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .take(IMAGE_ERROR_PREVIEW_CHARS)
        .collect();
    if value.chars().count() > IMAGE_ERROR_PREVIEW_CHARS {
        format!("{preview}...[truncated]")
    } else {
        preview
    }
}

fn validate_generated_image_url(raw: &str) -> Result<()> {
    anyhow::ensure!(
        raw.chars().take(IMAGE_URL_MAX_CHARS + 1).count() <= IMAGE_URL_MAX_CHARS,
        "generated image URL exceeds the {IMAGE_URL_MAX_CHARS}-character limit"
    );
    let parsed = url::Url::parse(raw).context("generated image URL was invalid")?;
    anyhow::ensure!(
        matches!(parsed.scheme(), "http" | "https") && parsed.host().is_some(),
        "generated image URL must use http or https and include a host"
    );
    anyhow::ensure!(
        parsed.username().is_empty() && parsed.password().is_none(),
        "generated image URL must not contain user credentials"
    );
    Ok(())
}

/// Validate the bytes before they are persisted or handed to a caller. The
/// image reader only has PNG/JPEG decoders enabled in this binary, matching
/// the image-generation contract, and applies strict dimension/allocation
/// limits before inspecting the header.
fn validate_generated_image(bytes: &[u8]) -> Result<&'static str> {
    anyhow::ensure!(!bytes.is_empty(), "generated image was empty");
    anyhow::ensure!(
        bytes.len() <= IMAGE_MAX_BYTES,
        "generated image exceeds the {IMAGE_MAX_BYTES}-byte limit"
    );
    let format = image::guess_format(bytes).context("generated image format was not recognized")?;
    let extension = match format {
        image::ImageFormat::Png => "png",
        image::ImageFormat::Jpeg => "jpg",
        other => {
            anyhow::bail!("generated image format {other:?} is not supported; expected PNG or JPEG")
        }
    };
    let mut reader = image::ImageReader::with_format(std::io::Cursor::new(bytes), format);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(IMAGE_MAX_EDGE);
    limits.max_image_height = Some(IMAGE_MAX_EDGE);
    limits.max_alloc = Some(IMAGE_MAX_PIXELS.saturating_mul(4));
    reader.limits(limits);
    let (width, height) = reader
        .into_dimensions()
        .context("generated image header or dimensions were invalid")?;
    anyhow::ensure!(
        width > 0 && height > 0,
        "generated image had zero dimensions"
    );
    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .context("generated image dimensions overflowed")?;
    anyhow::ensure!(
        pixels <= IMAGE_MAX_PIXELS,
        "generated image has {pixels} pixels; maximum is {IMAGE_MAX_PIXELS}"
    );
    Ok(extension)
}

/// Filesystem-safe stem from a prompt/name: lowercase alnum runs joined by
/// dashes, capped, never empty.
fn slug(text: &str) -> String {
    let mut out = String::new();
    let mut dash_pending = false;
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() {
            if dash_pending && !out.is_empty() {
                out.push('-');
            }
            dash_pending = false;
            out.push(ch.to_ascii_lowercase());
            if out.len() >= 48 {
                break;
            }
        } else {
            dash_pending = true;
        }
    }
    if out.is_empty() {
        "image".to_string()
    } else {
        out
    }
}

/// Atomically reserve `<stem>.<ext>`, then `<stem>-2.<ext>`… so concurrent
/// generations and dangling symlinks can never make a generation overwrite an
/// existing target.
fn create_unique_image(
    dir: &Path,
    stem: &str,
    extension: &str,
) -> Result<(PathBuf, std::fs::File)> {
    for index in 1..=1_000usize {
        let suffix = if index == 1 {
            String::new()
        } else {
            format!("-{index}")
        };
        let path = dir.join(format!("{stem}{suffix}.{extension}"));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options
                .mode(0o600)
                .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
        }
        match options.open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(error).with_context(|| format!("could not create {}", path.display()))
            }
        }
    }
    anyhow::bail!("could not reserve an image path for `{stem}` after 1000 attempts")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one_shot_image_server(status: &str, body: String) -> (String, std::thread::JoinHandle<()>) {
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
    async fn live_generation_failure_advances_to_configured_fallback() {
        let mut png = Vec::new();
        image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(2, 2, image::Rgb([8, 16, 32])))
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let encoded = base64::engine::general_purpose::STANDARD.encode(&png);
        let (failed_url, failed) = one_shot_image_server(
            "503 Service Unavailable",
            r#"{"error":{"message":"primary unavailable"}}"#.to_string(),
        );
        let (working_url, working) = one_shot_image_server(
            "200 OK",
            serde_json::json!({"data": [{"b64_json": encoded}]}).to_string(),
        );
        let config = ImageGenConfig {
            provider_id: "primary-fixture".into(),
            model: "primary-image".into(),
            base_url: failed_url,
            api_key: None,
            fallbacks: vec![ImageGenConfig {
                provider_id: "fallback-fixture".into(),
                model: "fallback-image".into(),
                base_url: working_url,
                api_key: None,
                fallbacks: vec![],
            }],
        };
        let generated = generate(&config, "fixture", Some("1024x1024"))
            .await
            .expect("fallback should generate the image");
        assert_eq!(generated, png);
        failed.join().unwrap();
        working.join().unwrap();
    }

    #[test]
    fn slugs_are_filesystem_safe_and_bounded() {
        assert_eq!(slug("A hero image: neon city!"), "a-hero-image-neon-city");
        assert_eq!(slug("///"), "image");
        assert!(slug(&"x".repeat(300)).len() <= 48);
    }

    #[test]
    fn unique_image_creation_never_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let (first, mut first_file) = create_unique_image(dir.path(), "hero", "png").unwrap();
        first_file.write_all(b"x").unwrap();
        drop(first_file);
        let (second, _second_file) = create_unique_image(dir.path(), "hero", "png").unwrap();
        assert_ne!(first, second);
        assert!(second.to_string_lossy().contains("hero-2"));
        assert_eq!(std::fs::read(first).unwrap(), b"x");
    }

    #[cfg(unix)]
    #[test]
    fn unique_image_creation_does_not_follow_dangling_symlinks() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        symlink(
            dir.path().join("missing-target"),
            dir.path().join("hero.png"),
        )
        .unwrap();
        let (path, _file) = create_unique_image(dir.path(), "hero", "png").unwrap();
        assert!(path.ends_with("hero-2.png"));
    }

    #[test]
    fn generated_image_validation_checks_signature_and_dimensions() {
        let image = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            2,
            3,
            image::Rgb([1, 2, 3]),
        ));
        let mut png = Vec::new();
        image
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        assert_eq!(validate_generated_image(&png).unwrap(), "png");
        assert!(validate_generated_image(b"not an image").is_err());

        let too_wide = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            IMAGE_MAX_EDGE + 1,
            1,
            image::Rgb([1, 2, 3]),
        ));
        let mut oversized = Vec::new();
        too_wide
            .write_to(
                &mut std::io::Cursor::new(&mut oversized),
                image::ImageFormat::Png,
            )
            .unwrap();
        assert!(validate_generated_image(&oversized).is_err());
    }

    #[test]
    fn generated_image_urls_are_bounded_and_http_only() {
        assert!(validate_generated_image_url("https://example.test/image.png").is_ok());
        assert!(validate_generated_image_url("file:///etc/passwd").is_err());
        assert!(validate_generated_image_url("https://user:pass@example.test/image.png").is_err());
        let too_long = format!("https://example.test/{}", "x".repeat(IMAGE_URL_MAX_CHARS));
        assert!(validate_generated_image_url(&too_long).is_err());
    }

    #[tokio::test]
    async fn oversized_prompt_fails_before_network_io() {
        let config = ImageGenConfig {
            provider_id: "fixture".into(),
            model: "fixture".into(),
            base_url: "https://example.invalid/v1".into(),
            api_key: None,
            fallbacks: vec![],
        };
        let error = generate(&config, &"x".repeat(IMAGE_PROMPT_MAX_CHARS + 1), None)
            .await
            .expect_err("oversized prompt must fail");
        assert!(error.to_string().contains("prompt exceeds"));
    }

    #[test]
    fn config_is_none_when_no_image_model() {
        let llm = crate::config::LLMProfile::new("openai", "gpt-4o");
        assert!(image_gen_config(&llm).unwrap().is_none());
        assert!(IMAGE_CONFIG_HELP.contains("Settings → Models & Providers → Image generation"));
        assert!(IMAGE_CONFIG_HELP.contains("primary provider/model"));
        assert!(IMAGE_CONFIG_HELP.contains("image fallbacks"));
    }
}
