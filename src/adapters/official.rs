use crate::{
    config::{Config, secret},
    engine::Engine,
    types::{Attachment, Channel, Event, Scene, now},
};
use anyhow::{Context, Result, bail};
use axum::{
    body::Bytes,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier};
use futures_util::{SinkExt, StreamExt};
use reqwest::Client;
use serde_json::Value;
use std::{sync::Arc, time::Duration};
use tokio::time::sleep;
use tokio_tungstenite::{connect_async, tungstenite::Message};

fn key(secret: &str) -> Result<SigningKey> {
    if !secret.is_ascii() || secret.is_empty() {
        bail!("QQ bot secret must contain ASCII characters")
    };
    let mut seed = Vec::new();
    while seed.len() < 32 {
        seed.extend_from_slice(secret.as_bytes());
    }
    seed.truncate(32);
    Ok(SigningKey::from_bytes(seed.as_slice().try_into()?))
}
fn verify(secret: &str, timestamp: &str, body: &[u8], header: &str) -> Result<bool> {
    if timestamp.len() > 32 || !timestamp.bytes().all(|c| c.is_ascii_digit()) {
        return Ok(false);
    }
    let raw = hex::decode(header)?;
    let sig = Signature::from_slice(&raw)?;
    let k = key(secret)?;
    let mut message = timestamp.as_bytes().to_vec();
    message.extend_from_slice(body);
    Ok(k.verifying_key().verify(&message, &sig).is_ok())
}
fn sign(secret: &str, message: &str) -> Result<String> {
    Ok(hex::encode(
        key(secret)?.sign(message.as_bytes()).to_bytes(),
    ))
}
pub async fn handle_webhook(
    cfg: &Config,
    engine: &Engine,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if body.len() > 2 * 1024 * 1024 {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            "request too large".to_owned(),
        )
            .into_response();
    }
    let payload: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return (StatusCode::BAD_REQUEST, "invalid JSON".to_owned()).into_response(),
    };
    let op = payload["op"].as_i64().unwrap_or(-1);
    if op == 13 {
        let plain = payload["d"]["plain_token"].as_str().unwrap_or("");
        let ts = payload["d"]["event_ts"].as_str().unwrap_or("");
        if plain.is_empty() || ts.is_empty() {
            return (StatusCode::BAD_REQUEST, "invalid validation".to_owned()).into_response();
        }
        let secret = std::env::var(&cfg.official.secret_env).unwrap_or_default();
        let mut message = ts.as_bytes().to_vec();
        message.extend_from_slice(plain.as_bytes());
        return match sign(&secret, std::str::from_utf8(&message).unwrap_or("")) {
            Ok(signature) => {
                axum::Json(serde_json::json!({"plain_token":plain,"signature":signature}))
                    .into_response()
            }
            Err(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "validation failed".to_owned(),
            )
                .into_response(),
        };
    }
    let timestamp = headers
        .get("x-signature-timestamp")
        .and_then(|x| x.to_str().ok())
        .unwrap_or("");
    let signature = headers
        .get("x-signature-ed25519")
        .and_then(|x| x.to_str().ok())
        .unwrap_or("");
    let secret = std::env::var(&cfg.official.secret_env).unwrap_or_default();
    if !verify(&secret, timestamp, &body, signature).unwrap_or(false) {
        return (StatusCode::UNAUTHORIZED, "invalid signature".to_owned()).into_response();
    }
    if op == 0 {
        if let Some(e) = parse_event(&payload) {
            if let Err(err) = engine.submit(e.clone()).await {
                engine
                    .store()
                    .trace(&e.session(), "webhook_queue_error", &err.to_string());
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "message queue unavailable".to_owned(),
                )
                    .into_response();
            }
        }
    }
    (
        StatusCode::OK,
        axum::Json(serde_json::json!({"op":12,"d":0})),
    )
        .into_response()
}
pub fn parse_event(p: &Value) -> Option<Event> {
    let kind = p["t"].as_str()?;
    let d = &p["d"];
    let (scene, target, sender, name, mention) = match kind {
        "C2C_MESSAGE_CREATE" => (
            Scene::Private,
            d["author"]["user_openid"].as_str()?,
            d["author"]["user_openid"].as_str()?.to_owned(),
            d["author"]["username"].as_str().unwrap_or("").to_owned(),
            false,
        ),
        "GROUP_AT_MESSAGE_CREATE" | "GROUP_MESSAGE_CREATE" => (
            Scene::Group,
            d["group_openid"].as_str()?,
            d["author"]["member_openid"].as_str()?.to_owned(),
            d["author"]["username"].as_str().unwrap_or("").to_owned(),
            kind == "GROUP_AT_MESSAGE_CREATE",
        ),
        "AT_MESSAGE_CREATE" | "MESSAGE_CREATE" => (
            Scene::Guild,
            d["channel_id"].as_str()?,
            d["author"]["id"].as_str()?.to_owned(),
            d["author"]["username"].as_str().unwrap_or("").to_owned(),
            kind == "AT_MESSAGE_CREATE",
        ),
        "DIRECT_MESSAGE_CREATE" => (
            Scene::GuildDm,
            d["guild_id"].as_str()?,
            d["author"]["id"].as_str()?.to_owned(),
            d["author"]["username"].as_str().unwrap_or("").to_owned(),
            true,
        ),
        _ => return None,
    };
    if d["author"]["bot"].as_bool() == Some(true) {
        return None;
    }
    let mut text = d["content"].as_str().unwrap_or("").to_string();
    let mut attachments = Vec::new();
    if let Some(xs) = d["attachments"].as_array() {
        for a in xs {
            if let Some(url) = a["url"].as_str() {
                let mime = a["content_type"].as_str().unwrap_or("");
                attachments.push(Attachment {
                    kind: if mime.starts_with("audio/") {
                        "audio"
                    } else if mime.starts_with("image/") {
                        "image"
                    } else {
                        "file"
                    }
                    .into(),
                    url: url.into(),
                    name: a["filename"]
                        .as_str()
                        .or_else(|| a["content_type"].as_str())
                        .unwrap_or("")
                        .into(),
                    sticker: false,
                });
            }
        }
    }
    if let Some(els) = d["msg_elements"].as_array() {
        for el in els {
            if let Some(s) = el["content"].as_str() {
                if !text.is_empty() {
                    text.push('\n')
                }
                text.push_str(s)
            }
            if let Some(asr) = el["asr_refer_text"].as_str() {
                attachments.push(Attachment {
                    kind: "audio".into(),
                    url: String::new(),
                    name: format!("ASR:{asr}"),
                    sticker: false,
                });
            }
        }
    }
    let rid = d["id"].as_str().map(str::to_owned);
    Some(Event {
        id: rid
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
        channel: Channel::Official,
        scene,
        target: target.into(),
        sender,
        sender_name: name,
        text,
        mentioned: mention,
        timestamp: chrono::DateTime::parse_from_rfc3339(d["timestamp"].as_str().unwrap_or(""))
            .map(|t| t.timestamp())
            .unwrap_or_else(|_| now()),
        attachments,
        reply_to: rid,
        proactive: false,
    })
}

pub async fn gateway(cfg: Arc<Config>, engine: Engine) {
    let http = Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .unwrap();
    let mut delay = 1u64;
    loop {
        let result = gateway_once(&cfg, &engine, &http).await;
        if let Err(err) = result {
            tracing::warn!(error=%err,"QQ official gateway disconnected")
        };
        sleep(Duration::from_secs(delay)).await;
        delay = (delay * 2).min(60);
    }
}
async fn gateway_once(cfg: &Config, engine: &Engine, http: &Client) -> Result<()> {
    let secret = secret(&cfg.official.secret_env)?;
    let token = fetch_token(cfg, &secret, http).await?;
    let v: Value = http
        .get(format!(
            "{}/gateway",
            cfg.official.base_url.trim_end_matches('/')
        ))
        .bearer_auth(&token)
        .send()
        .await?
        .json()
        .await?;
    let url = v["url"]
        .as_str()
        .context("QQ gateway response has no URL")?;
    let (mut ws, _) = connect_async(url).await?;
    let mut heartbeat = tokio::time::interval(Duration::from_secs(45));
    let mut identify = false;
    loop {
        tokio::select! {msg=ws.next()=>{let Some(msg)=msg else{break};let msg=msg?;if !msg.is_text(){continue}let p:Value=serde_json::from_str(msg.to_text()?)?;let op=p["op"].as_i64().unwrap_or(-1);match op{10=>{let ms=p["d"]["heartbeat_interval"].as_u64().unwrap_or(45000).max(1000);heartbeat=tokio::time::interval(Duration::from_millis(ms));heartbeat.tick().await;if !identify{let shards=v["shards"].as_u64().unwrap_or(1);ws.send(Message::Text(serde_json::json!({"op":2,"d":{"token":format!("QQBot {token}"),"intents":(1i64<<9)|(1i64<<12)|(1i64<<25)|(1i64<<30),"shard":[0,shards]}}).to_string().into())).await?;identify=true;}},11=>{},0=>{if let Some(e)=parse_event(&p){let _=engine.submit(e).await;}},7|9=>break,_=>{} }},_=heartbeat.tick(),if identify=>{ws.send(Message::Text(serde_json::json!({"op":1,"d":null}).to_string().into())).await?;}}
    }
    Ok(())
}
async fn fetch_token(cfg: &Config, secret: &str, http: &Client) -> Result<String> {
    let v: Value = http
        .post(&cfg.official.token_url)
        .json(&serde_json::json!({"appId":cfg.official.app_id,"clientSecret":secret}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(v["access_token"]
        .as_str()
        .context("QQ token response lacks access_token")?
        .to_owned())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn signature_validation_roundtrips() {
        let secret = "abcdefghijklmnopqrstuvwxyz123456";
        let ts = "1234567890";
        let body = b"{\"op\":0}";
        let sig = sign(secret, &format!("{ts}{}", String::from_utf8_lossy(body))).unwrap();
        assert!(verify(secret, ts, body, &sig).unwrap());
        assert!(!verify(secret, "1234567891", body, &sig).unwrap());
    }
    #[test]
    fn parses_group_message_without_using_qq_numbers() {
        let p = serde_json::json!({"t":"GROUP_AT_MESSAGE_CREATE","d":{"id":"m1","group_openid":"go","content":"hi","timestamp":"2026-01-01T00:00:00Z","author":{"member_openid":"uo","username":"friend"}}});
        let e = parse_event(&p).unwrap();
        assert_eq!(e.target, "go");
        assert_eq!(e.sender, "uo");
    }
}
