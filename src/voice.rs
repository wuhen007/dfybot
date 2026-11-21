use crate::{
    config::{Config, secret},
    types::Attachment,
};
use anyhow::{Context, Result, bail};
use futures_util::StreamExt;
use reqwest::Client;
use serde_json::Value;
use std::{process::Stdio, time::Duration};
use tokio::{io::AsyncWriteExt, process::Command};

#[derive(Clone)]
pub struct VoiceClient {
    http: Client,
}
impl VoiceClient {
    pub fn new() -> Result<Self> {
        Ok(Self {
            http: Client::builder()
                .timeout(Duration::from_secs(45))
                .pool_max_idle_per_host(4)
                .build()?,
        })
    }
    pub async fn transcribe(&self, c: &Config, a: &Attachment) -> Result<String> {
        if let Some(t) = a.name.strip_prefix("ASR:") {
            return Ok(t.to_string());
        }
        let bytes = self.download(c, a).await?;
        let key = secret(&c.voice.api_key_env)?;
        let url = format!(
            "{}/audio/transcriptions",
            c.voice.base_url.trim_end_matches('/')
        );
        let part = reqwest::multipart::Part::bytes(bytes)
            .file_name(if a.name.is_empty() {
                "voice.wav".into()
            } else {
                a.name.clone()
            })
            .mime_str(&mime(&a.name))?;
        let form = reqwest::multipart::Form::new()
            .text("model", c.voice.asr_model.clone())
            .part("file", part);
        let r = self
            .http
            .post(url)
            .bearer_auth(key)
            .multipart(form)
            .send()
            .await
            .context("speech recognition request")?;
        let status = r.status();
        let v: Value = r.json().await.context("decode ASR response")?;
        if !status.is_success() {
            bail!("ASR returned {status}")
        };
        let text = v["text"].as_str().unwrap_or("").trim().to_string();
        if text.is_empty() {
            bail!("ASR returned empty text")
        };
        Ok(text)
    }
    async fn download(&self, c: &Config, a: &Attachment) -> Result<Vec<u8>> {
        let u = reqwest::Url::parse(&a.url)?;
        if u.scheme() != "https"
            || !c
                .voice
                .media_hosts
                .iter()
                .any(|h| h.eq_ignore_ascii_case(u.host_str().unwrap_or("")))
        {
            bail!("voice URL host is not in voice.media_hosts")
        }
        let response = self.http.get(u).send().await?.error_for_status()?;
        if response
            .content_length()
            .is_some_and(|len| len > c.voice.max_bytes as u64)
        {
            bail!("voice attachment is too large")
        };
        let mut stream = response.bytes_stream();
        let mut bytes = Vec::new();
        while let Some(chunk) = stream.next().await {
            append_limited(&mut bytes, &chunk?, c.voice.max_bytes)?;
        }
        Ok(bytes)
    }
    pub async fn synthesize(&self, c: &Config, text: &str) -> Result<Vec<u8>> {
        if !c.voice.enabled {
            bail!("voice is disabled")
        };
        let key = secret(&c.voice.api_key_env)?;
        let url = format!("{}/audio/speech", c.voice.base_url.trim_end_matches('/'));
        let r=self.http.post(url).bearer_auth(key).json(&serde_json::json!({"model":c.voice.tts_model,"voice":c.voice.tts_voice,"input":text,"response_format":c.voice.tts_format})).send().await?;
        let status = r.status();
        if !status.is_success() {
            bail!("TTS returned {status}")
        };
        let b = r.bytes().await?;
        if b.is_empty() || b.len() > c.voice.max_bytes {
            bail!("TTS response is empty or too large")
        };
        Ok(b.to_vec())
    }
    pub async fn send_onebot(
        &self,
        c: &Config,
        http: &Client,
        target: &str,
        group: bool,
        text: &str,
    ) -> Result<()> {
        let audio = self.synthesize(c, text).await?;
        if c.voice.silk_encoder.is_empty() {
            bail!("configure voice.silk_encoder to send OneBot voice messages")
        };
        let mut ff = Command::new(if c.voice.ffmpeg.is_empty() {
            "ffmpeg"
        } else {
            &c.voice.ffmpeg
        });
        ff.args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-i",
            "pipe:0",
            "-f",
            "wav",
            "-ac",
            "1",
            "-ar",
            "24000",
            "pipe:1",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
        let mut child = ff
            .spawn()
            .context("start ffmpeg")
            .map_err(|_| anyhow::anyhow!("configure ffmpeg to convert TTS audio to WAV"))?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(&audio).await?;
        }
        let wav = child.wait_with_output().await?;
        if !wav.status.success() || wav.stdout.is_empty() {
            bail!("ffmpeg could not convert TTS audio")
        };
        let mut silk = Command::new(&c.voice.silk_encoder)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("start SILK encoder")?;
        if let Some(mut stdin) = silk.stdin.take() {
            stdin.write_all(&wav.stdout).await?;
        }
        let result = silk.wait_with_output().await?;
        if !result.status.success() || result.stdout.is_empty() {
            bail!("SILK encoder failed")
        };
        let output = result.stdout;
        let url = if group {
            format!("{}/send_group_msg", c.onebot.http_url.trim_end_matches('/'))
        } else {
            format!(
                "{}/send_private_msg",
                c.onebot.http_url.trim_end_matches('/')
            )
        };
        let seg = serde_json::json!([{"type":"record","data":{"file":format!("base64://{}",base64::Engine::encode(&base64::engine::general_purpose::STANDARD,output))}}]);
        let mut req = http.post(url).json(&if group {
            serde_json::json!({"group_id":target,"message":seg})
        } else {
            serde_json::json!({"user_id":target,"message":seg})
        });
        if !c.onebot.token_env.is_empty() {
            if let Ok(t) = std::env::var(&c.onebot.token_env) {
                req = req.header("Authorization", format!("Bearer {t}"));
            }
        }
        let r = req.send().await?;
        if !r.status().is_success() {
            bail!("OneBot rejected voice message: {}", r.status())
        };
        let v: Value = r.json().await?;
        if v["status"] == "failed" {
            bail!("OneBot rejected voice message")
        };
        Ok(())
    }
}
pub fn mime(name: &str) -> String {
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "silk" => "audio/silk",
        "amr" => "audio/amr",
        "ogg" => "audio/ogg",
        "opus" => "audio/opus",
        _ => "application/octet-stream",
    }
    .into()
}
pub fn audio_segment(v: &Value) -> Option<Attachment> {
    if v["type"] != "record" && v["type"] != "audio" {
        return None;
    };
    let d = &v["data"];
    let file = d["url"].as_str().or_else(|| d["file"].as_str())?;
    let name = d["file"].as_str().unwrap_or("voice.amr");
    if name.contains("://") && !name.starts_with("http") {
        return None;
    };
    Some(Attachment {
        kind: "audio".into(),
        url: file.into(),
        name: name.into(),
        sticker: false,
    })
}

fn append_limited(output: &mut Vec<u8>, chunk: &[u8], max_bytes: usize) -> Result<()> {
    if chunk.len() > max_bytes.saturating_sub(output.len()) {
        bail!("voice attachment is too large")
    }
    output.extend_from_slice(chunk);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn audio_mime_recognizes_supported_types() {
        assert_eq!(mime("voice.amr"), "audio/amr");
        assert_eq!(mime("voice.ogg"), "audio/ogg");
    }
    #[test]
    fn rejects_non_audio_segments() {
        assert!(
            audio_segment(&serde_json::json!({"type":"image","data":{"url":"https://x"}}))
                .is_none()
        );
    }

    #[test]
    fn streamed_audio_obeys_size_limit_across_chunks() {
        let mut output = Vec::new();
        append_limited(&mut output, b"123", 5).unwrap();
        assert!(append_limited(&mut output, b"456", 5).is_err());
        assert_eq!(output, b"123");
    }
}
