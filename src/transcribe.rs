use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use tokio::process::Command;
use transcribe_rs::{
    SpeechModel, TranscribeOptions,
    onnx::{Quantization, parakeet::ParakeetModel},
};
use uuid::Uuid;

use crate::models::AttachmentTranscript;

const HANDY_MODEL_DIR_NAME: &str = "parakeet-tdt-0.6b-v3-int8";

pub fn detect_handy_parakeet_model_dir() -> Option<PathBuf> {
    handy_model_roots()
        .into_iter()
        .map(|root| root.join(HANDY_MODEL_DIR_NAME))
        .find(|candidate| is_valid_parakeet_model_dir(candidate))
}

pub async fn transcribe_audio_file(
    model_dir: PathBuf,
    source_path: PathBuf,
    scratch_dir: PathBuf,
) -> Result<AttachmentTranscript> {
    let wav_path = scratch_dir.join(format!("{}.wav", Uuid::now_v7()));
    convert_audio_to_wav(&source_path, &wav_path).await?;

    let wav_for_transcription = wav_path.clone();
    let transcript_result = tokio::task::spawn_blocking(move || -> Result<AttachmentTranscript> {
        let mut model = ParakeetModel::load(&model_dir, &Quantization::Int8)
            .with_context(|| format!("failed to load Handy model from {}", model_dir.display()))?;
        let result = model
            .transcribe_file(&wav_for_transcription, &TranscribeOptions::default())
            .with_context(|| format!("failed to transcribe {}", wav_for_transcription.display()))?;
        let text = result.text.trim().to_string();
        if text.is_empty() {
            bail!("transcript is empty");
        }
        Ok(AttachmentTranscript {
            engine: "Handy Parakeet".to_string(),
            text,
        })
    })
    .await
    .context("audio transcription task join failed")?;

    let _ = fs::remove_file(&wav_path);
    transcript_result
}

async fn convert_audio_to_wav(source_path: &Path, wav_path: &Path) -> Result<()> {
    let output = Command::new("ffmpeg")
        .arg("-y")
        .arg("-i")
        .arg(source_path)
        .arg("-ac")
        .arg("1")
        .arg("-ar")
        .arg("16000")
        .arg("-c:a")
        .arg("pcm_s16le")
        .arg(wav_path)
        .output()
        .await
        .with_context(|| format!("failed to spawn ffmpeg for {}", source_path.display()))?;

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if stderr.is_empty() {
        bail!("ffmpeg exited with status {}", output.status);
    }
    bail!("ffmpeg exited with status {}: {stderr}", output.status);
}

fn handy_model_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(appdata) = std::env::var_os("APPDATA") {
        roots.push(
            PathBuf::from(&appdata)
                .join("com.pais.handy")
                .join("models"),
        );
    }
    if let Some(local_appdata) = std::env::var_os("LOCALAPPDATA") {
        roots.push(
            PathBuf::from(local_appdata)
                .join("Handy")
                .join("resources")
                .join("models"),
        );
    }
    roots
}

fn is_valid_parakeet_model_dir(dir: &Path) -> bool {
    [
        "encoder-model.int8.onnx",
        "decoder_joint-model.int8.onnx",
        "nemo128.onnx",
        "vocab.txt",
    ]
    .iter()
    .all(|name| dir.join(name).is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_incomplete_model_dir() {
        let temp = tempfile::tempdir().unwrap();
        assert!(!is_valid_parakeet_model_dir(temp.path()));
    }
}
