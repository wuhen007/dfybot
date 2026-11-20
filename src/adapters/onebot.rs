use crate::{
    config::Config,
    engine::Engine,
    types::{Attachment, Channel, Event, Scene, now},
};
use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use std::{sync::Arc, time::Duration};
use tokio::time::sleep;
use tokio_tungstenite::{
    connect_async,
    tungstenite::{Message, client::IntoClientRequest},
};

pub async fn run(cfg: Arc<Config>, engine: Engine) {
    let mut delay = 1;
    loop {
        if let Err(e) = connect(&cfg, &engine).await {
            tracing::warn!(error=%e,"OneBot connection ended");
        }
        sleep(Duration::from_secs(delay)).await;
        delay = (delay * 2).min(60);
    }
}
async fn connect(cfg: &Config, engine: &Engine) -> Result<()> {
    let mut req = cfg.onebot.ws_url.clone().into_client_request()?;
    if !cfg.onebot.token_env.is_empty() {
        if let Ok(token) = std::env::var(&cfg.onebot.token_env) {
            req.headers_mut()
                .insert("Authorization", format!("Bearer {token}").parse()?);
        }
    }
    let (ws, _) = connect_async(req)
        .await
        .context("connect OneBot reverse websocket")?;
    tracing::info!("OneBot websocket connected");
    let (mut sink, mut stream) = ws.split();
    while let Some(message) = stream.next().await {
        match message? {
            Message::Text(data) => {
                if let Ok(v) = serde_json::from_str::<Value>(&data) {
                    if let Some(e) = parse_event(&v, &cfg.onebot.bot_id) {
                        if let Err(err) = engine.submit(e).await {
                            engine
                                .store()
                                .trace("onebot", "queue_error", &err.to_string());
                        }
                    }
                }
            }
            Message::Ping(data) => sink.send(Message::Pong(data)).await?,
            Message::Close(_) => break,
            _ => {}
        }
    }
    Ok(())
}
fn parse_event(v: &Value, bot_id: &str) -> Option<Event> {
    let kind = v["post_type"].as_str()?;
    if kind != "message" {
        return None;
    }
    let scene = match v["message_type"].as_str()? {
        "private" => Scene::Private,
        "group" => Scene::Group,
        _ => return None,
    };
    let sender = v["sender"]["user_id"]
        .as_i64()
        .map(|x| x.to_string())
        .or_else(|| v["user_id"].as_i64().map(|x| x.to_string()))?;
    if !bot_id.is_empty() && sender == bot_id {
        return None;
    }
    let target = if scene == Scene::Group {
        v["group_id"].as_i64().map(|x| x.to_string())?
    } else {
        sender.clone()
    };
    let mut text = String::new();
    let mut attachments = Vec::new();
    let mut mentioned = false;
    let segs = if v["message"].is_array() {
        v["message"].as_array()?.clone()
    } else {
        vec![
            serde_json::json!({"type":"text","data":{"text":v["raw_message"].as_str().unwrap_or("")}}),
        ]
    };
    for seg in segs {
        match seg["type"].as_str().unwrap_or("") {
            "text" => text.push_str(seg["data"]["text"].as_str().unwrap_or("")),
            "at" => {
                let id = seg["data"]["qq"]
                    .as_str()
                    .or_else(|| seg["data"]["qq"].as_i64().map(|_| ""))
                    .unwrap_or("");
                if id == bot_id {
                    mentioned = true;
                }
            }
            "record" => {
                if let Some(url) = seg["data"]["url"]
                    .as_str()
                    .or_else(|| seg["data"]["file"].as_str())
                {
                    attachments.push(Attachment {
                        kind: "audio".into(),
                        url: url.into(),
                        name: seg["data"]["file"].as_str().unwrap_or("voice.amr").into(),
                        sticker: false,
                    });
                }
            }
            "image" => {
                if let Some(url) = seg["data"]["url"].as_str() {
                    attachments.push(Attachment {
                        kind: "image".into(),
                        url: url.into(),
                        name: String::new(),
                        sticker: false,
                    });
                }
            }
            "face" | "mface" => attachments.push(Attachment {
                kind: "sticker".into(),
                url: seg["data"]["url"].as_str().unwrap_or("").into(),
                name: String::new(),
                sticker: true,
            }),
            _ => {}
        }
    }
    Some(Event {
        id: v["message_id"]
            .as_i64()
            .map(|x| x.to_string())
            .or_else(|| v["message_id"].as_str().map(str::to_string))
            .unwrap_or_else(|| format!("{}-{}", sender, now())),
        channel: Channel::Onebot,
        scene,
        target,
        sender,
        sender_name: v["sender"]["card"]
            .as_str()
            .or_else(|| v["sender"]["nickname"].as_str())
            .unwrap_or("")
            .into(),
        text,
        mentioned,
        timestamp: v["time"].as_i64().unwrap_or_else(now),
        attachments,
        reply_to: None,
        proactive: false,
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn normalizes_onebot_group_message() {
        let v = serde_json::json!({"post_type":"message","message_type":"group","group_id":44,"user_id":7,"message_id":9,"message":[{"type":"at","data":{"qq":"99"}},{"type":"text","data":{"text":"hello"}}]});
        let e = parse_event(&v, "99").unwrap();
        assert_eq!(e.target, "44");
        assert!(e.mentioned);
        assert_eq!(e.text, "hello");
    }
    #[test]
    fn ignores_bot_self_messages() {
        let v = serde_json::json!({"post_type":"message","message_type":"private","user_id":99,"message":"hi"});
        assert!(parse_event(&v, "99").is_none());
    }
}
