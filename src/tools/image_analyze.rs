//! Inspect disk images through the acting model's native image transport,
//! or the configured vision sidecar when native delivery is unavailable.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct ImageAnalyzeInput {
    /// Path to the image — absolute, or relative to the workspace.
    pub path: String,
    /// What to find out. Omit for a thorough description (contents, all
    /// visible text verbatim, layout, notable details).
    #[serde(default)]
    pub question: Option<String>,
    /// Optional detail region in original image pixels. Source remains unchanged.
    #[serde(default)]
    pub crop: Option<crate::runtime::vision::ImageRegion>,
    /// Whole reference images retained beside later observations in this turn.
    #[serde(default)]
    pub reference_paths: Vec<String>,
}

const SUPPORTED: [&str; 5] = ["png", "jpg", "jpeg", "webp", "gif"];
const IMAGE_PATH_MAX_CHARS: usize = 4_096;
const IMAGE_QUESTION_MAX_CHARS: usize = 16_384;

/// Resolve the image path: absolute as-is, otherwise workspace-relative.
fn resolve(workspace: &Path, raw: &str) -> PathBuf {
    let candidate = PathBuf::from(raw);
    if candidate.is_absolute() {
        candidate
    } else {
        workspace.join(candidate)
    }
}

pub(crate) fn validated_path(workspace: &Path, input: &ImageAnalyzeInput) -> Result<PathBuf> {
    if input.reference_paths.len() > 2 {
        bail!("At most two reference images can accompany one primary image");
    }
    if input.path.chars().take(IMAGE_PATH_MAX_CHARS + 1).count() > IMAGE_PATH_MAX_CHARS {
        bail!("image path exceeds the {IMAGE_PATH_MAX_CHARS}-character limit");
    }
    if input.question.as_deref().is_some_and(|question| {
        question.chars().take(IMAGE_QUESTION_MAX_CHARS + 1).count() > IMAGE_QUESTION_MAX_CHARS
    }) {
        bail!("image question exceeds the {IMAGE_QUESTION_MAX_CHARS}-character limit");
    }
    let path = resolve(workspace, &input.path);
    if !path.is_file() {
        bail!(
            "image not found: {} — pass an absolute path or one relative to the workspace",
            path.display()
        );
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if !SUPPORTED.contains(&ext.as_str()) {
        bail!(
            "unsupported image type `.{ext}` ({}) — supported: {}",
            path.display(),
            SUPPORTED.join(", ")
        );
    }
    Ok(path)
}

/// Native delivery is selected by the acting turn, never inferred from the
/// company default model. The mesh attaches pixels before reporting success.
pub(crate) fn prepare_native(workspace: &Path, input: ImageAnalyzeInput) -> Result<String> {
    let path = validated_path(workspace, &input)?;
    let (width, height) = crate::runtime::vision::image_source_dimensions(&path)?;
    let mut labels = format!("\nRequested primary image: {path:?}");
    for reference in &input.reference_paths {
        let reference = validated_path(
            workspace,
            &ImageAnalyzeInput {
                path: reference.clone(),
                question: None,
                crop: None,
                reference_paths: vec![],
            },
        )?;
        labels.push_str(&format!("; explicit reference {reference:?} (whole image)"));
    }
    labels.push_str("\nThe attached image message supplies the actual deduplicated order. Explicit reference_paths replace the retained reference set for this turn after successful delivery. A primary-only inspection updates only the current observation.");
    let detail=input.crop.map(|r|format!("\nDetail crop in original image pixels: x={}, y={}, width={}, height={}. This view covers only that region; inspect the full image separately. Do not use cropped coordinates for desktop actions.",r.x,r.y,r.width,r.height)).unwrap_or_default();
    Ok(format!(
        "Image selected for native inspection: {}\nOriginal image dimensions: {width}x{height} pixels.{detail}{labels}\nInspection question: {}",
        path.display(),
        input
            .question
            .as_deref()
            .unwrap_or("Describe the image and inspect visible defects.")
    ))
}

pub(crate) async fn native_attachment(
    workspace: &Path,
    input: ImageAnalyzeInput,
) -> Result<String> {
    let path = validated_path(workspace, &input)?;
    match input.crop {
        Some(region) => crate::runtime::vision::image_region_data_uri(&path, region).await,
        None => crate::runtime::vision::screenshot_data_uri(&path).await,
    }
}

/// Prepare a single, ordered comparison set. Any invalid reference rejects
/// the whole set; never tell the model it saw pixels that were dropped.
pub(crate) async fn native_attachments(
    workspace: &Path,
    mut input: ImageAnalyzeInput,
) -> Result<crate::runtime::vision::NativeImageInspection> {
    use crate::runtime::vision::{NativeImage, NativeImageInspection};
    let primary_path = validated_path(workspace, &input)?;
    let crop = input.crop;
    let references = std::mem::take(&mut input.reference_paths);
    let primary = NativeImage::file(&primary_path, crop, native_attachment(workspace, input).await?);
    let mut images = Vec::new();
    for path in references {
        let reference_input = ImageAnalyzeInput { path, question: None, crop: None, reference_paths: vec![] };
        let path = validated_path(workspace, &reference_input)?;
        images.push(NativeImage::reference(&path, native_attachment(workspace, reference_input).await?));
    }
    Ok(NativeImageInspection { primary, references: images })
}

/// Called only after the tool executor authorizes and prepares this inspection.
/// A failed replacement never leaves the previous observation looking current.
pub(crate) async fn attach_native(
    workspace: &Path,
    input: ImageAnalyzeInput,
    images: &mut crate::runtime::vision::NativeTurnImages,
) -> Result<()> {
    images.invalidate_current();
    images.set_inspection(native_attachments(workspace, input).await?)
}

pub async fn execute(workspace: &Path, input: ImageAnalyzeInput) -> Result<String> {
    let path = validated_path(workspace, &input)?;
    if !input.reference_paths.is_empty() {
        bail!("Comparing reference images requires native vision on the acting model; no reference images were analyzed");
    }
    let config = crate::config::PhoenixConfig::load().context("config unreadable")?;
    let vision = crate::runtime::vision::vision_config(&config.profile.llm)
        .context("vision lane unresolvable")?
        .context(
            "no vision model configured — `phoenix configure` → Model roles → Vision enables image_analyze",
        )?;
    let answer = match input.crop {
        Some(region) => {
            crate::runtime::vision::analyze_image_region(
                &vision,
                &path,
                input.question.as_deref(),
                region,
            )
            .await
        }
        None => {
            crate::runtime::vision::analyze_image_file(&vision, &path, input.question.as_deref())
                .await
        }
    }
    .with_context(|| format!("vision call failed for {}", path.display()))?;
    Ok(format!("[{}]\n{}", path.display(), answer.trim()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn comparison_delivers_primary_crop_and_whole_references_in_order() {
        use base64::Engine;
        let dir = tempfile::tempdir().unwrap();
        for (name, color) in [
            ("primary.png", [220, 10, 10]),
            ("reference.png", [10, 220, 10]),
            ("detail.png", [10, 10, 220]),
        ] {
            image::RgbImage::from_pixel(20, 12, image::Rgb(color))
                .save(dir.path().join(name))
                .unwrap();
        }
        let input = || {
            serde_json::from_value::<ImageAnalyzeInput>(serde_json::json!({
            "path":"primary.png", "crop":{"x":2,"y":3,"width":4,"height":5},
            "reference_paths":["reference.png","detail.png"], "question":"Compare shape and color"
        })).unwrap()
        };
        let receipt = prepare_native(dir.path(), input()).unwrap();
        assert!(receipt.contains("Requested primary image:"));
        assert!(receipt.contains("explicit reference"));
        let source = std::fs::read(dir.path().join("primary.png")).unwrap();
        let mut state = crate::runtime::vision::NativeTurnImages::default();
        state.set_inspection(native_attachments(dir.path(), input()).await.unwrap()).unwrap();
        let message = state.message().unwrap();
        assert!(message.content.contains("1 = primary image"));
        assert!(message.content.contains("2 = reference"));
        assert!(message.content.contains("3 = reference"));
        let images = message.images;
        assert_eq!(images.len(), 3);
        for (index, (uri, color)) in images
            .iter()
            .zip([[220, 10, 10], [10, 220, 10], [10, 10, 220]])
            .enumerate()
        {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(uri.split_once(',').unwrap().1)
                .unwrap();
            let pixels = image::load_from_memory(&bytes).unwrap().to_rgb8();
            assert_eq!(
                pixels.dimensions(),
                if index == 0 { (4, 5) } else { (20, 12) }
            );
            // Whole-image transport uses bounded JPEG; crops remain lossless.
            let tolerance = if index == 0 { 0 } else { 3 };
            assert!(pixels.pixels().all(|p| p.0.into_iter().zip(color).all(|(actual, expected)| actual.abs_diff(expected) <= tolerance)), "image {index} color order changed: {:?}", pixels.get_pixel(0,0));
        }
        assert_eq!(
            std::fs::read(dir.path().join("primary.png")).unwrap(),
            source
        );
        assert!(execute(dir.path(), input())
            .await
            .unwrap_err()
            .to_string()
            .contains("requires native vision"));
        std::fs::write(dir.path().join("detail.png"), b"broken image").unwrap();
        assert!(
            native_attachments(dir.path(), input()).await.is_err(),
            "invalid second reference cannot silently disappear"
        );
        let mut too_many = input();
        too_many.reference_paths.push("reference.png".into());
        assert!(native_attachments(dir.path(), too_many)
            .await
            .unwrap_err()
            .to_string()
            .contains("At most two"));
    }
    #[tokio::test]
    async fn detail_crop_delivers_exact_pixels_and_preserves_original() {
        use base64::Engine;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("detail.png");
        let mut source = image::RgbImage::from_pixel(20, 12, image::Rgb([0, 0, 220]));
        for x in 8..12 {
            for y in 3..6 {
                source.put_pixel(x, y, image::Rgb([170, 40, 20]));
            }
        }
        source.save(&path).unwrap();
        let before = std::fs::read(&path).unwrap();
        let input = || {
            serde_json::from_value::<ImageAnalyzeInput>(serde_json::json!({"path":"detail.png","crop":{"x":8,"y":3,"width":4,"height":3},"question":"Inspect the stem"})).unwrap()
        };
        let receipt = prepare_native(dir.path(), input()).unwrap();
        assert!(receipt.contains("20x12"));
        assert!(receipt.contains("x=8, y=3"));
        let uri = native_attachment(dir.path(), input()).await.unwrap();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(uri.split_once(',').unwrap().1)
            .unwrap();
        let pixels = image::load_from_memory(&bytes).unwrap().to_rgb8();
        assert_eq!(pixels.dimensions(), (4, 3));
        assert!(pixels.pixels().all(|p| p.0 == [170, 40, 20]));
        assert_eq!(std::fs::read(&path).unwrap(), before);
        for crop in [
            serde_json::json!({"x":19,"y":0,"width":2,"height":1}),
            serde_json::json!({"x":0,"y":0,"width":0,"height":1}),
            serde_json::json!({"x":4294967295u32,"y":0,"width":2,"height":1}),
        ] {
            let input =
                serde_json::from_value(serde_json::json!({"path":"detail.png","crop":crop}))
                    .unwrap();
            assert!(native_attachment(dir.path(), input).await.is_err());
        }
        assert!(serde_json::from_value::<ImageAnalyzeInput>(
            serde_json::json!({"path":"detail.png","crop":{"x":-1,"y":0,"width":1,"height":1}})
        )
        .is_err());
    }

    #[tokio::test]
    async fn native_image_decodes_pixels_without_a_vision_model() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("render.png");
        image::RgbImage::from_pixel(32, 24, image::Rgb([25, 140, 80]))
            .save(&path)
            .unwrap();
        let input = || ImageAnalyzeInput {
            crop: None,
            reference_paths: vec![],
            path: "render.png".into(),
            question: Some("Inspect the render".into()),
        };
        assert!(prepare_native(dir.path(), input())
            .unwrap()
            .contains("Inspect the render"));
        let payload = native_attachment(dir.path(), input()).await.unwrap();
        assert!(payload.starts_with("data:image/"));
        assert!(payload.contains(";base64,"));
        std::fs::write(path, "not an image").unwrap();
        assert!(
            native_attachment(dir.path(), input()).await.is_err(),
            "invalid pixels cannot count as inspection"
        );
    }

    #[test]
    fn resolve_handles_absolute_and_relative() {
        let ws = Path::new("/tmp/ws");
        assert_eq!(resolve(ws, "/abs/pic.png"), PathBuf::from("/abs/pic.png"));
        assert_eq!(
            resolve(ws, "shots/pic.png"),
            PathBuf::from("/tmp/ws/shots/pic.png")
        );
    }

    #[tokio::test]
    async fn missing_file_and_bad_extension_fail_loud() {
        let dir = tempfile::tempdir().unwrap();
        let missing = execute(
            dir.path(),
            ImageAnalyzeInput {
                crop: None,
                reference_paths: vec![],
                path: "nope.png".into(),
                question: None,
            },
        )
        .await;
        assert!(missing.is_err(), "missing file must error");
        assert!(format!("{:#}", missing.unwrap_err()).contains("image not found"));

        let doc = dir.path().join("notes.txt");
        std::fs::write(&doc, "text").unwrap();
        let wrong = execute(
            dir.path(),
            ImageAnalyzeInput {
                crop: None,
                reference_paths: vec![],
                path: doc.display().to_string(),
                question: None,
            },
        )
        .await;
        assert!(wrong.is_err(), "non-image must error");
        assert!(format!("{:#}", wrong.unwrap_err()).contains("unsupported image type"));
    }

    #[tokio::test]
    async fn oversized_path_and_question_fail_before_config_or_network() {
        let dir = tempfile::tempdir().unwrap();
        let path_error = execute(
            dir.path(),
            ImageAnalyzeInput {
                crop: None,
                reference_paths: vec![],
                path: "x".repeat(IMAGE_PATH_MAX_CHARS + 1),
                question: None,
            },
        )
        .await
        .expect_err("oversized path must fail");
        assert!(path_error.to_string().contains("path exceeds"));

        let question_error = execute(
            dir.path(),
            ImageAnalyzeInput {
                crop: None,
                reference_paths: vec![],
                path: "unused.png".into(),
                question: Some("x".repeat(IMAGE_QUESTION_MAX_CHARS + 1)),
            },
        )
        .await
        .expect_err("oversized question must fail");
        assert!(question_error.to_string().contains("question exceeds"));
    }
}
