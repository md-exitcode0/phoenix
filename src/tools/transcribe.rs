//! transcribe_audio: speech-to-text for a local audio or video file, through
//! the shared STT lane in `crate::voice` (Groq Whisper by default).
use super::ToolOutput;
use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Deserialize)]
pub struct TranscribeInput {
    /// Path to the audio/video file (workspace-relative or absolute).
    pub path: String,
}

pub async fn execute(path: PathBuf) -> Result<ToolOutput> {
    let meta = std::fs::metadata(&path).with_context(|| format!("cannot read {}", path.display()))?;
    anyhow::ensure!(meta.is_file(), "{} is not a file", path.display());
    anyhow::ensure!(
        meta.len() as usize <= crate::voice::MAX_AUDIO_BYTES,
        "{} is larger than 25 MB; cut it into shorter parts (for example with ffmpeg -f segment) and transcribe each",
        path.display()
    );
    let bytes = std::fs::read(&path)?;
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("audio.ogg").to_string();
    let text = crate::voice::transcribe(bytes, &name).await?;
    Ok(ToolOutput {
        summary: format!("transcribed {} ({} characters)", name, text.chars().count()),
        content: text,
    })
}
