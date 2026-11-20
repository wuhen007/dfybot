use crate::{
    config::Config,
    model::{ModelClient, ToolCall},
    store::{MemoryKind, ProactiveAction, Settings, Store},
    types::{Channel, Event, Scene, now, truncate},
    voice::VoiceClient,
};
use anyhow::{Result, bail};
use reqwest::Client;
use serde_json::Value;
use sha2::Digest;
use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
    time::Duration,
};
use tokio::{
    sync::{Mutex, Semaphore, mpsc},
    time::sleep,
};
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
pub struct Engine {
    cfg: Arc<Config>,
    store: Store,
    model: ModelClient,
    voice: VoiceClient,
    client: Client,
    permit: Arc<Semaphore>,
    settings: Arc<Mutex<Settings>>,
    queue: Arc<Mutex<Option<Vec<mpsc::Sender<Event>>>>>,
    active: Arc<Mutex<HashMap<String, CancellationToken>>>,
}
impl Engine {
    pub fn new(cfg: Arc<Config>, store: Store) -> Result<Self> {
        let settings = store.settings(Settings {
            enabled: cfg.chat.enabled,
            dry_run: cfg.chat.dry_run,
            persona: cfg.chat.persona.clone(),
            group_mode: cfg.chat.group_mode.clone(),
            voice_replies: cfg.chat.voice_replies,
            proactive: cfg.chat.proactive,
            context_messages: cfg.chat.context_messages,
            awake_start: cfg.chat.awake_start,
            awake_end: cfg.chat.awake_end,
            utc_offset_hours: cfg.chat.utc_offset_hours,
            proactive_interval_minutes: cfg.chat.proactive_interval_minutes,
            proactive_sessions: cfg.chat.proactive_sessions.clone(),
            proactive_goals: cfg.chat.proactive_goals.clone(),
            night_message: cfg.chat.night_message,
            night_message_hour: cfg.chat.night_message_hour,
            group_keywords: cfg.chat.group_keywords.clone(),
            auto_memory: cfg.chat.auto_memory,
            memory_limit: cfg.chat.memory_limit,
            memory_recall: cfg.chat.memory_recall,
            cooldown_secs: cfg.chat.cooldown_secs,
        });
        Ok(Self {
            permit: Arc::new(Semaphore::new(8)),
            cfg,
            store,
            model: ModelClient::new()?,
            voice: VoiceClient::new()?,
            client: Client::builder()
                .timeout(Duration::from_secs(20))
                .pool_max_idle_per_host(16)
                .build()?,
            settings: Arc::new(Mutex::new(settings)),
            queue: Arc::new(Mutex::new(None)),
            active: Arc::new(Mutex::new(HashMap::new())),
        })
    }
    pub fn store(&self) -> &Store {
        &self.store
    }
    pub fn config(&self) -> &Config {
        &self.cfg
    }
    pub fn voice(&self) -> &VoiceClient {
        &self.voice
    }
    pub fn settings_handle(&self) -> Arc<Mutex<Settings>> {
        self.settings.clone()
    }
    pub async fn interrupt(&self, key: &str) -> bool {
        let active = self.active.lock().await;
        if let Some(token) = active.get(key) {
            token.cancel();
            true
        } else {
            false
        }
    }
    pub async fn interject(&self, key: &str, text: &str) -> Result<()> {
        let (channel, scene, target) = Store::session_parts(key)?;
        let owner =
            self.cfg.chat.owners.first().ok_or_else(|| {
                anyhow::anyhow!("configure at least one owner account to interject")
            })?;
        let event = Event {
            id: format!("panel-{}", uuid::Uuid::new_v4()),
            channel: match channel.as_str() {
                "onebot" => Channel::Onebot,
                "official" => Channel::Official,
                _ => bail!("unsupported channel"),
            },
            scene: match scene.as_str() {
                "private" => Scene::Private,
                "group" => Scene::Group,
                "guild" => Scene::Guild,
                "guild_dm" => Scene::GuildDm,
                _ => bail!("unsupported scene"),
            },
            target,
            sender: owner.clone(),
            sender_name: "主人".into(),
            text: truncate(text, 4000),
            mentioned: true,
            timestamp: now(),
            attachments: vec![],
            reply_to: None,
            proactive: false,
        };
        self.submit(event).await
    }
    pub async fn start_workers(&self) {
        let mut slot = self.queue.lock().await;
        if slot.is_some() {
            return;
        }
        let mut senders = Vec::new();
        for _ in 0..self.cfg.server.workers {
            let (tx, rx) = mpsc::channel(self.cfg.server.queue_capacity / self.cfg.server.workers);
            senders.push(tx);
            let e = self.clone();
            tokio::spawn(worker_loop(e, rx));
        }
        *slot = Some(senders);
    }
    pub async fn submit(&self, e: Event) -> Result<()> {
        let token = self.access(&e)?;
        let settings = self.settings.lock().await.clone();
        let keyword_hit = settings
            .group_keywords
            .iter()
            .any(|w| !w.is_empty() && e.text.contains(w));
        if !token
            || (!e.is_private()
                && !e.proactive
                && match settings.group_mode.as_str() {
                    "mention" => !e.mentioned,
                    "keyword" => !e.mentioned && !keyword_hit,
                    _ => false,
                })
        {
            return Ok(());
        };
        if !self.store.claim_event(&e)? {
            return Ok(());
        }
        let slot = self.queue.lock().await;
        let senders = slot
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("chat workers have not started"))?;
        let index = e.shard(senders.len());
        match senders[index].try_send(e.clone()) {
            Ok(()) => Ok(()),
            Err(err) => {
                self.store.release_event(&e);
                Err(anyhow::anyhow!("message queue is full: {err}"))
            }
        }
    }
    fn access(&self, e: &Event) -> Result<bool> {
        let own = self.cfg.chat.owners.iter().any(|x| x == &e.sender);
        let allowed = self.cfg.chat.allowed_sessions.is_empty()
            || self.cfg.chat.allowed_sessions.contains(&e.session())
            || own;
        if !allowed {
            self.store
                .trace(&e.session(), "access_denied", "message ignored");
            return Ok(false);
        };
        Ok(true)
    }
    async fn process(&self, e: Event) -> Result<()> {
        let key = e.session();
        let cancel = CancellationToken::new();
        self.active.lock().await.insert(key.clone(), cancel.clone());
        let result = tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                self.store.trace(&key, "interrupted", "reply cancelled by operator");
                Ok(())
            }
            result = self.process_event(e) => result,
        };
        self.active.lock().await.remove(&key);
        result
    }

    async fn process_event(&self, mut e: Event) -> Result<()> {
        let settings = self.settings.lock().await.clone();
        let owner = self.cfg.chat.owners.iter().any(|x| x == &e.sender);
        let permit = self.permit.clone().acquire_owned().await?;
        let text = tokio::time::timeout(Duration::from_secs(45), self.prepare_text(&e))
            .await
            .map_err(|_| anyhow::anyhow!("voice recognition timed out"))??;
        drop(permit);
        if text.trim().is_empty() {
            return Ok(());
        }
        e.text = truncate(&text, self.cfg.chat.max_input_chars);
        self.store.trace(
            &e.session(),
            "message",
            &format!("{}: {}", e.sender, e.text),
        );
        if !settings.enabled && !e.proactive {
            if !(owner && e.is_private() && ["/active", "/status"].contains(&e.text.as_str())) {
                return Ok(());
            }
        }
        if let Some(signal) = mood_signal(&e.text) {
            self.store.affect(&e.session(), signal);
        }
        let mode = self.cfg.chat.session_models.get(&e.session());
        let _ = mode;
        if !self.cfg.chat.owners.is_empty() && owner && e.is_private() {
            if e.text.starts_with("/remember ") {
                if Store::permission_allowed(self.cfg.chat.memory_permission, owner, true) {
                    self.store.add_memory(
                        "*",
                        &e.sender,
                        &e.text[10..],
                        3,
                        settings.memory_limit,
                    )?;
                    self.send_text(&e, "已经记下了。").await?;
                }
                return Ok(());
            }
            if e.text == "/status" {
                self.send_text(
                    &e,
                    if settings.enabled {
                        "在线"
                    } else {
                        "暂停接话"
                    },
                )
                .await?;
                return Ok(());
            }
            if e.text == "/silent" {
                let mut x = self.settings.lock().await;
                x.enabled = false;
                self.store.set_settings(&x)?;
                self.send_text(&e, "已安静下来。").await?;
                return Ok(());
            }
            if e.text == "/active" {
                let mut x = self.settings.lock().await;
                x.enabled = true;
                self.store.set_settings(&x)?;
                self.send_text(&e, "我回来啦。").await?;
                return Ok(());
            }
            if e.text.starts_with("/persona ") {
                let name = e.text[9..].trim();
                let ps = self.store.personas(&self.cfg.chat.personas)?;
                if ps.contains_key(name) {
                    self.store.set_session_persona(&e.session(), Some(name))?;
                    self.send_text(&e, &format!("这个会话会使用「{name}」人格。"))
                        .await?;
                } else {
                    self.send_text(&e, "找不到这个人格。").await?;
                }
                return Ok(());
            }
            if e.text == "/forget" {
                self.store.forget_session(&e.session())?;
                self.send_text(&e, "已清除当前会话的记忆和对话记录。")
                    .await?;
                return Ok(());
            }
        }
        if !e.proactive
            && !self
                .store
                .claim_cooldown(&e.session(), &e.sender, settings.cooldown_secs)?
        {
            self.store.trace(&e.session(), "rate_limited", &e.sender);
            return Ok(());
        }
        if settings.dry_run {
            self.store.trace(
                &e.session(),
                "dry_run",
                &format!("would answer: {}", e.text),
            );
            return Ok(());
        }
        let _p = self.permit.clone().acquire_owned().await?;
        let reply = tokio::time::timeout(
            Duration::from_secs(self.cfg.model.timeout_secs + 5),
            self.generate_reply(&e, &settings),
        )
        .await
        .map_err(|_| anyhow::anyhow!("model response timed out"))??;
        drop(_p);
        let reply = truncate(&reply, self.cfg.chat.max_reply_chars);
        if reply.trim().is_empty() {
            return Ok(());
        };
        if e.proactive {
            self.store
                .record_proactive(&e, &reply, settings.context_messages)?;
        } else {
            self.store
                .record(&e, &e.text, &reply, settings.context_messages)?;
        }
        self.store.trace(&e.session(), "reply", &reply);
        if settings.auto_memory && e.is_private() && !e.proactive {
            let engine = self.clone();
            let event = e.clone();
            if let Ok(permit) = self.permit.clone().try_acquire_owned() {
                tokio::spawn(async move {
                    let _permit = permit;
                    match engine
                        .model
                        .extract_memories(&engine.cfg, &engine.store, &event)
                        .await
                    {
                        Ok(mut memories) if !memories.is_empty() => {
                            let owner = engine.cfg.chat.owners.iter().any(|id| id == &event.sender);
                            if owner {
                                for memory in &mut memories {
                                    memory.session = "*".into();
                                }
                            }
                            let count = memories.len();
                            if let Err(err) = engine
                                .store
                                .add_memories(&memories, engine.cfg.chat.memory_limit)
                            {
                                engine.store.trace(
                                    &event.session(),
                                    "memory_error",
                                    &err.to_string(),
                                );
                            } else {
                                engine.store.trace(
                                    &event.session(),
                                    "memory_saved",
                                    &format!("saved {count} memories"),
                                );
                            }
                        }
                        Ok(_) => {}
                        Err(err) => {
                            engine
                                .store
                                .trace(&event.session(), "memory_error", &err.to_string())
                        }
                    }
                });
            } else {
                self.store
                    .trace(&e.session(), "memory_skipped", "model is busy");
            }
        }
        let (reply, sticker) = extract_sticker(&reply);
        if let Some(id) = sticker {
            if self.send_sticker(&e, id).await.is_err() {
                self.store.trace(
                    &e.session(),
                    "sticker_fallback",
                    "sticker was unavailable; continued with text",
                );
            }
        }
        let parts = crate::types::chunks(reply.trim(), 360, self.cfg.chat.max_reply_parts);
        let mut first = true;
        for part in parts {
            if settings.voice_replies && self.cfg.voice.enabled && first {
                if self.send_voice(&e, &part).await.is_ok() {
                    first = false;
                    continue;
                }
                self.store.trace(
                    &e.session(),
                    "voice_fallback",
                    "TTS unavailable; using text",
                );
            }
            self.send_text(&e, &part).await?;
            first = false;
        }
        Ok(())
    }
    async fn generate_reply(&self, e: &Event, settings: &Settings) -> Result<String> {
        let mut messages = self
            .model
            .conversation(&self.cfg, &self.store, e, &e.text)?;
        let owner = self.cfg.chat.owners.iter().any(|id| id == &e.sender);
        let allow_remember = !settings.dry_run
            && Store::permission_allowed(self.cfg.chat.memory_permission, owner, e.is_private());
        let tools = ModelClient::tool_schemas(
            allow_remember,
            owner && e.is_private(),
            self.cfg.chat.stickers,
        );
        for round in 0..3 {
            let completion = self.model.complete(&self.cfg, &messages, &tools).await?;
            if completion.calls.is_empty() {
                return Ok(completion.text);
            }
            if round == 2 {
                bail!("model exceeded the tool round limit")
            }
            messages.push(completion.assistant_message);
            for call in completion.calls {
                let result = self.execute_tool(e, settings, owner, &call).await;
                match result {
                    Ok(value) => {
                        self.store.trace(&e.session(), "tool", &call.name);
                        messages.push(serde_json::json!({"role":"tool","tool_call_id":call.id,"content":value}));
                    }
                    Err(err) => {
                        self.store.trace(&e.session(), "tool_denied", &call.name);
                        messages.push(serde_json::json!({"role":"tool","tool_call_id":call.id,"content":err.to_string()}));
                    }
                }
            }
        }
        bail!("model did not return a final response")
    }
    async fn execute_tool(
        &self,
        e: &Event,
        settings: &Settings,
        owner: bool,
        call: &ToolCall,
    ) -> Result<String> {
        let args = &call.arguments;
        let output = match call.name.as_str() {
            "search_memory" => {
                let query = args["query"].as_str().unwrap_or("");
                serde_json::to_string(&self.store.recall(
                    e,
                    query,
                    settings.memory_recall,
                    owner && e.is_private(),
                )?)?
            }
            "remember" => {
                if settings.dry_run
                    || !Store::permission_allowed(
                        self.cfg.chat.memory_permission,
                        owner,
                        e.is_private(),
                    )
                {
                    bail!("memory writes are not allowed in this conversation")
                }
                let kind = match args["kind"].as_str().unwrap_or("fact") {
                    "fact" => MemoryKind::Fact,
                    "profile" => MemoryKind::Profile,
                    "experience" => MemoryKind::Experience,
                    "style" => MemoryKind::Style,
                    _ => bail!("unknown memory category"),
                };
                let subject = args["subject"].as_str().unwrap_or("").trim();
                let text = args["text"].as_str().unwrap_or("").trim();
                if subject.is_empty() || subject.len() > 100 || text.is_empty() || text.len() > 500
                {
                    bail!("memory fields are empty or too long")
                }
                let scope = if owner
                    && e.is_private()
                    && self.cfg.chat.memory_permission == crate::config::Permission::OwnerPrivate
                {
                    "*".to_owned()
                } else {
                    e.session()
                };
                self.store.add_typed_memory(
                    &scope,
                    kind,
                    subject,
                    text,
                    args["importance"].as_u64().unwrap_or(2).min(5) as u8,
                    settings.memory_limit,
                )?;
                "记忆已保存".to_owned()
            }
            "search_stickers" => {
                if !self.cfg.chat.stickers {
                    bail!("stickers are disabled")
                }
                let rows = self
                    .store
                    .sticker_matches(args["query"].as_str().unwrap_or(""), 8)?;
                let safe: Vec<_> = rows
                    .into_iter()
                    .map(|row| {
                        serde_json::json!({"id":row["id"],"name":row["name"],"tags":row["tags"]})
                    })
                    .collect();
                serde_json::to_string(&safe)?
            }
            "send_sticker" => {
                if !self.cfg.chat.stickers || settings.dry_run {
                    bail!("sticker sending is disabled")
                }
                let id = args["id"]
                    .as_i64()
                    .ok_or_else(|| anyhow::anyhow!("sticker id is required"))?;
                self.send_sticker(e, id).await?;
                "表情包已发送".to_owned()
            }
            "set_persona" => {
                if settings.dry_run || !owner || !e.is_private() {
                    bail!("persona changes require an owner private conversation")
                }
                let name = args["name"].as_str().unwrap_or("").trim();
                if !self
                    .store
                    .personas(&self.cfg.chat.personas)?
                    .contains_key(name)
                {
                    bail!("unknown persona")
                }
                self.store.set_session_persona(&e.session(), Some(name))?;
                "人格已切换，下一轮生效".to_owned()
            }
            _ => bail!("unknown tool"),
        };
        Ok(output)
    }
    async fn prepare_text(&self, e: &Event) -> Result<String> {
        let mut text = e.text.clone();
        let mut transcripts = vec![];
        for a in &e.attachments {
            if a.sticker {
                if self.cfg.chat.stickers && !a.url.is_empty() {
                    let hash = hex::encode(sha2::Sha256::digest(a.url.as_bytes()));
                    let _ = self.store.save_sticker(&e.sender, &hash, &a.url, &a.name);
                }
            } else if a.kind == "audio" {
                match self.voice.transcribe(&self.cfg, a).await {
                    Ok(t) => transcripts.push(t),
                    Err(err) => self
                        .store
                        .trace(&e.session(), "asr_error", &err.to_string()),
                }
            } else if a.kind == "image" && self.cfg.model.vision {
                let u = reqwest::Url::parse(&a.url)?;
                if u.scheme() == "https"
                    && self
                        .cfg
                        .voice
                        .media_hosts
                        .iter()
                        .any(|h| h.eq_ignore_ascii_case(u.host_str().unwrap_or("")))
                {
                    text.push_str("\n[图片] ");
                    text.push_str(&a.url);
                }
            }
        }
        for t in transcripts {
            text.push_str("\n[语音识别] ");
            text.push_str(&t);
        }
        Ok(text)
    }
    async fn send_text(&self, e: &Event, text: &str) -> Result<()> {
        if self.settings.lock().await.dry_run {
            return Ok(());
        }
        match e.channel {
            Channel::Onebot => {
                let url = if e.scene == Scene::Group {
                    "send_group_msg"
                } else {
                    "send_private_msg"
                };
                let body = if e.scene == Scene::Group {
                    serde_json::json!({"group_id":e.target,"message":text})
                } else {
                    serde_json::json!({"user_id":e.target,"message":text})
                };
                let mut r = self
                    .client
                    .post(format!(
                        "{}/{url}",
                        self.cfg.onebot.http_url.trim_end_matches('/')
                    ))
                    .json(&body);
                if !self.cfg.onebot.token_env.is_empty() {
                    if let Ok(t) = std::env::var(&self.cfg.onebot.token_env) {
                        r = r.bearer_auth(t)
                    }
                }
                let res = r.send().await?;
                if !res.status().is_success() {
                    bail!("OneBot send failed: {}", res.status())
                }
            }
            Channel::Official => {
                if !e.proactive && now() - e.timestamp > self.cfg.official.reply_window_secs {
                    bail!("QQ official passive reply window expired")
                };
                let url = match e.scene {
                    Scene::Private => format!("/v2/users/{}/messages", e.target),
                    Scene::Group => format!("/v2/groups/{}/messages", e.target),
                    Scene::Guild => format!("/channels/{}/messages", e.target),
                    Scene::GuildDm => format!("/dms/{}/messages", e.target),
                };
                let token = self.official_token().await?;
                let mut b = serde_json::json!({"content":text,"msg_type":0});
                if let Some(id) = &e.reply_to {
                    b["msg_id"] = id.clone().into();
                    b["msg_seq"] = 1.into();
                }
                let r = self
                    .client
                    .post(format!(
                        "{}{}",
                        self.cfg.official.base_url.trim_end_matches('/'),
                        url
                    ))
                    .bearer_auth(token)
                    .json(&b)
                    .send()
                    .await?;
                if !r.status().is_success() {
                    let status = r.status();
                    let body = r.text().await.unwrap_or_default();
                    bail!("QQ official send failed {status}: {}", truncate(&body, 200));
                }
            }
            Channel::Panel => {}
        }
        Ok(())
    }
    async fn send_voice(&self, e: &Event, text: &str) -> Result<()> {
        match e.channel {
            Channel::Onebot => {
                self.voice
                    .send_onebot(
                        &self.cfg,
                        &self.client,
                        &e.target,
                        e.scene == Scene::Group,
                        text,
                    )
                    .await
            }
            Channel::Official => {
                if !["group", "private"].contains(&match e.scene {
                    Scene::Group => "group",
                    Scene::Private => "private",
                    _ => "other",
                }) {
                    bail!("QQ official voice is supported in C2C/group only")
                };
                let audio = self.voice.synthesize(&self.cfg, text).await?;
                if self.cfg.voice.tts_format != "opus" {
                    bail!("QQ official bot requires Opus audio for voice upload")
                };
                let token = self.official_token().await?;
                let (base, path) = match e.scene {
                    Scene::Private => (
                        &self.cfg.official.base_url,
                        format!("/v2/users/{}/files", e.target),
                    ),
                    Scene::Group => (
                        &self.cfg.official.base_url,
                        format!("/v2/groups/{}/files", e.target),
                    ),
                    _ => unreachable!(),
                };
                let upload=self.client.post(format!("{}{}",base.trim_end_matches('/'),path)).bearer_auth(&token).json(&serde_json::json!({"file_type":3,"srv_send_msg":false,"file_data":base64::Engine::encode(&base64::engine::general_purpose::STANDARD,&audio)})).send().await?;
                let status = upload.status();
                let v: Value = upload.json().await?;
                if !status.is_success() {
                    bail!("QQ voice upload failed: {status}")
                };
                let info = v["file_info"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("QQ voice upload returned no file_info"))?;
                let url = if e.scene == Scene::Private {
                    format!("/v2/users/{}/messages", e.target)
                } else {
                    format!("/v2/groups/{}/messages", e.target)
                };
                let mut body =
                    serde_json::json!({"msg_type":7,"media":{"file_info":info},"msg_seq":1});
                if let Some(id) = &e.reply_to {
                    body["msg_id"] = id.clone().into();
                    body["msg_seq"] = 1.into();
                }
                let send = self
                    .client
                    .post(format!("{}{url}", base.trim_end_matches('/')))
                    .bearer_auth(token)
                    .json(&body)
                    .send()
                    .await?;
                if !send.status().is_success() {
                    bail!("QQ official voice send failed: {}", send.status())
                };
                Ok(())
            }
            Channel::Panel => Ok(()),
        }
    }
    async fn send_sticker(&self, e: &Event, id: i64) -> Result<()> {
        let Some((_source, url, _name)) = self.store.sticker(id)? else {
            bail!("unknown sticker")
        };
        match e.channel {
            Channel::Onebot => {
                let path = if e.scene == Scene::Group {
                    "send_group_msg"
                } else {
                    "send_private_msg"
                };
                let body = if e.scene == Scene::Group {
                    serde_json::json!({"group_id":e.target,"message":[{"type":"image","data":{"file":url}}]})
                } else {
                    serde_json::json!({"user_id":e.target,"message":[{"type":"image","data":{"file":url}}]})
                };
                let response = self
                    .client
                    .post(format!(
                        "{}/{path}",
                        self.cfg.onebot.http_url.trim_end_matches('/')
                    ))
                    .json(&body)
                    .send()
                    .await?;
                if !response.status().is_success() {
                    bail!("OneBot sticker send failed")
                }
            }
            Channel::Official => {
                let token = self.official_token().await?;
                let path = match e.scene {
                    Scene::Private => format!("/v2/users/{}/files", e.target),
                    Scene::Group => format!("/v2/groups/{}/files", e.target),
                    _ => bail!("stickers are not supported in this QQ scene"),
                };
                let r = self
                    .client
                    .post(format!(
                        "{}{}",
                        self.cfg.official.base_url.trim_end_matches('/'),
                        path
                    ))
                    .bearer_auth(&token)
                    .json(&serde_json::json!({"file_type":1,"srv_send_msg":false,"url":url}))
                    .send()
                    .await?;
                let status = r.status();
                let v: Value = r.json().await?;
                if !status.is_success() {
                    bail!("QQ sticker upload failed")
                };
                let info = v["file_info"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("QQ sticker upload has no file_info"))?;
                let path = if e.scene == Scene::Private {
                    format!("/v2/users/{}/messages", e.target)
                } else {
                    format!("/v2/groups/{}/messages", e.target)
                };
                let r=self.client.post(format!("{}{}",self.cfg.official.base_url.trim_end_matches('/'),path)).bearer_auth(token).json(&serde_json::json!({"msg_type":7,"media":{"file_info":info},"msg_id":e.reply_to,"msg_seq":1})).send().await?;
                if !r.status().is_success() {
                    bail!("QQ sticker send failed")
                }
            }
            Channel::Panel => {}
        }
        self.store.use_sticker(id)?;
        Ok(())
    }
    async fn official_token(&self) -> Result<String> {
        if let Some((token, expiry)) = OFFICIAL_TOKEN.lock().await.clone() {
            if expiry > now() + 60 {
                return Ok(token);
            }
        }
        let secret = crate::config::secret(&self.cfg.official.secret_env)?;
        let response = self
            .client
            .post(&self.cfg.official.token_url)
            .json(&serde_json::json!({"appId":self.cfg.official.app_id,"clientSecret":secret}))
            .send()
            .await?;
        let status = response.status();
        let v: Value = response.json().await?;
        if !status.is_success() {
            bail!("QQ token endpoint returned {status}")
        };
        let token = v["access_token"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("QQ token response has no access_token"))?
            .to_string();
        let exp = now() + v["expires_in"].as_i64().unwrap_or(7200);
        *OFFICIAL_TOKEN.lock().await = Some((token.clone(), exp));
        Ok(token)
    }
}
static OFFICIAL_TOKEN: tokio::sync::Mutex<Option<(String, i64)>> =
    tokio::sync::Mutex::const_new(None);
pub async fn run_scheduler(engine: Engine, stop: CancellationToken) {
    let mut tick = tokio::time::interval(Duration::from_secs(60));
    loop {
        tokio::select! {
            _ = stop.cancelled() => break,
            _ = tick.tick() => {
                let settings = engine.settings.lock().await.clone();
                if !settings.enabled || !settings.proactive || settings.dry_run
                    || settings.proactive_sessions.is_empty() || engine.cfg.chat.owners.is_empty() {
                    continue;
                }
                let local_timestamp = now() + i64::from(settings.utc_offset_hours) * 3600;
                let hour = local_timestamp.rem_euclid(86400) as u32 / 3600;
                let local_day = chrono::DateTime::<chrono::Utc>::from_timestamp(local_timestamp, 0)
                    .map(|time| time.format("%Y-%m-%d").to_string())
                    .unwrap_or_default();
                let night_hour = settings.night_message && hour == settings.night_message_hour;
                if !night_hour && !is_awake(hour, settings.awake_start, settings.awake_end) {
                    continue;
                }
                let Ok(sessions) = engine.store.sessions(500) else { continue; };
                for session in sessions {
                    if !settings.proactive_sessions.contains(&session.key) { continue; }
                    let Ok((channel, scene, target)) = Store::session_parts(&session.key) else { continue; };
                    let scene = match scene.as_str() {
                        "private" => Scene::Private,
                        "guild_dm" => Scene::GuildDm,
                        _ => continue,
                    };
                    let Some(owner) = engine.store.last_user_sender(&session.key).ok().flatten() else { continue; };
                    if !proactive_target_allowed(&scene, &target, &owner, &engine.cfg.chat.owners) { continue; }
                    let channel = match channel.as_str() {
                        "onebot" => Channel::Onebot,
                        "official" => Channel::Official,
                        _ => continue,
                    };
                    let event = Event {
                        id: uuid::Uuid::new_v4().to_string(),
                        channel,
                        scene,
                        target: target.clone(),
                        sender: owner,
                        sender_name: String::new(),
                        text: String::new(),
                        mentioned: false,
                        timestamp: now(),
                        attachments: vec![],
                        reply_to: None,
                        proactive: true,
                    };
                    let last = engine.store.history(&event, 1).unwrap_or_default();
                    let idle_seconds = last.first().map(|message| now() - message.timestamp).unwrap_or(0);
                    if (night_hour && idle_seconds < 30 * 60)
                        || (!night_hour && idle_seconds < settings.proactive_interval_minutes * 60) {
                        continue;
                    }
                    let action = engine.store.claim_proactive(
                        &session.key,
                        settings.proactive_interval_minutes,
                        if night_hour { Some(&local_day) } else { None },
                        settings.proactive_goals.len(),
                    );
                    let Ok(Some(action)) = action else { continue; };
                    let mut event = event;
                    event.text = match action {
                        ProactiveAction::Night => "现在向主人道晚安。结合你对主人的心情，用温暖、简短、不追问的语气收尾，不提及内部状态。".to_owned(),
                        ProactiveAction::Goal(index) => format!(
                            "主动搭话目标：{}。结合当前私聊最近的内容自然开场，不要提及任务或内部规则。",
                            settings.proactive_goals.get(index).map(String::as_str).unwrap_or("轻松问候主人"),
                        ),
                    };
                    if let Err(error) = engine.process(event.clone()).await {
                        engine.store.trace(&event.session(), "scheduler_error", &error.to_string());
                    }
                }
            }
        }
    }
}
fn is_awake(hour: u32, start: u32, end: u32) -> bool {
    if start == end {
        false
    } else if start < end {
        start <= hour && hour < end
    } else {
        hour >= start || hour < end
    }
}
fn proactive_target_allowed(scene: &Scene, target: &str, sender: &str, owners: &[String]) -> bool {
    let identity = if *scene == Scene::Private {
        target
    } else {
        sender
    };
    matches!(scene, Scene::Private | Scene::GuildDm) && owners.iter().any(|owner| owner == identity)
}
pub async fn worker_loop(engine: Engine, mut rx: mpsc::Receiver<Event>) {
    let mut pending = VecDeque::new();
    loop {
        let mut e = match pending.pop_front() {
            Some(e) => e,
            None => match rx.recv().await {
                Some(e) => e,
                None => break,
            },
        };
        if e.scene == Scene::Group && engine.cfg.chat.group_debounce_ms > 0 {
            sleep(Duration::from_millis(engine.cfg.chat.group_debounce_ms)).await;
            while let Ok(next) = rx.try_recv() {
                pending.push_back(next)
            }
            let mut rest = VecDeque::new();
            while let Some(next) = pending.pop_front() {
                if next.session() == e.session() {
                    e.text
                        .push_str(&format!("\n{}: {}", next.sender_name, next.text));
                    e.attachments.extend(next.attachments);
                } else {
                    rest.push_back(next)
                }
            }
            pending = rest;
        }
        if let Err(err) = engine.process(e.clone()).await {
            engine
                .store
                .trace(&e.session(), "handler_error", &err.to_string());
        }
    }
}
fn extract_sticker(text: &str) -> (String, Option<i64>) {
    let Some(start) = text.rfind("[[sticker:") else {
        return (text.to_owned(), None);
    };
    let Some(end_rel) = text[start..].find("]]") else {
        return (text.to_owned(), None);
    };
    let end = start + end_rel + 2;
    let id = text[start + 10..end - 2].trim().parse::<i64>().ok();
    let mut clean = text.to_owned();
    clean.replace_range(start..end, " ");
    (clean, id)
}

fn mood_signal(text: &str) -> Option<f32> {
    if [
        "不喜欢",
        "讨厌",
        "别再",
        "不要再",
        "难过",
        "伤心",
        "生气",
        "失望",
    ]
    .iter()
    .any(|signal| text.contains(signal))
    {
        Some(-0.35)
    } else if [
        "喜欢",
        "最爱",
        "谢谢",
        "开心",
        "高兴",
        "真棒",
        "太好了",
        "爱你",
    ]
    .iter()
    .any(|signal| text.contains(signal))
    {
        Some(0.35)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Permission;
    use std::path::Path;

    #[test]
    fn negative_mood_signals_take_precedence_over_positive_substrings() {
        assert_eq!(mood_signal("我不喜欢这个"), Some(-0.35));
        assert_eq!(mood_signal("谢谢你"), Some(0.35));
        assert_eq!(mood_signal("我的生日是今天"), None);
    }

    #[tokio::test]
    async fn mood_feedback_does_not_depend_on_automatic_memory() {
        let mut cfg = Config::default();
        cfg.chat.enabled = true;
        cfg.chat.dry_run = true;
        cfg.chat.auto_memory = false;
        let store = Store::open(Path::new(":memory:")).unwrap();
        let engine = Engine::new(Arc::new(cfg), store.clone()).unwrap();
        let event = Event {
            id: "mood-test".into(),
            channel: Channel::Onebot,
            scene: Scene::Private,
            target: "user-1".into(),
            sender: "user-1".into(),
            sender_name: "User".into(),
            text: "谢谢你，我很开心".into(),
            mentioned: false,
            timestamp: now(),
            attachments: vec![],
            reply_to: None,
            proactive: false,
        };
        let session = event.session();
        engine.process_event(event).await.unwrap();
        assert!(store.mood(&session) > 0.0);
    }

    #[tokio::test]
    async fn interrupt_stops_processing_and_cleans_up_the_session() {
        let cfg = Arc::new(Config::default());
        let store = Store::open(Path::new(":memory:")).unwrap();
        let engine = Engine::new(cfg, store).unwrap();
        let held_permits = engine.permit.clone().acquire_many_owned(8).await.unwrap();
        let event = Event {
            id: "interrupt-test".into(),
            channel: Channel::Onebot,
            scene: Scene::Private,
            target: "user-1".into(),
            sender: "user-1".into(),
            sender_name: "User".into(),
            text: "hello".into(),
            mentioned: false,
            timestamp: now(),
            attachments: vec![],
            reply_to: None,
            proactive: false,
        };
        let key = event.session();
        let worker = engine.clone();
        let task = tokio::spawn(async move { worker.process(event).await });
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if engine.active.lock().await.contains_key(&key) {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("request should become active");

        assert!(engine.interrupt(&key).await);
        assert!(task.await.unwrap().is_ok());
        assert!(!engine.active.lock().await.contains_key(&key));
        assert!(
            engine
                .store
                .traces(10)
                .unwrap()
                .iter()
                .any(|trace| trace.kind == "interrupted" && trace.session == key)
        );
        drop(held_permits);
    }

    #[test]
    fn permission_is_denied_for_group_by_default() {
        assert!(!Store::permission_allowed(
            Permission::OwnerPrivate,
            true,
            false
        ));
    }
    #[test]
    fn sticker_marker_is_removed_and_parsed() {
        let (text, id) = extract_sticker("Here [[sticker:42]] now");
        assert_eq!(id, Some(42));
        assert_eq!(text, "Here   now");
        assert_eq!(extract_sticker("no marker").1, None);
    }
    #[test]
    fn awake_window_supports_overnight_ranges() {
        assert!(is_awake(9, 8, 23));
        assert!(!is_awake(23, 8, 23));
        assert!(is_awake(23, 22, 6));
        assert!(is_awake(5, 22, 6));
        assert!(!is_awake(12, 22, 6));
        assert!(!is_awake(12, 12, 12));
    }
    #[test]
    fn proactive_targets_are_owner_private_only() {
        let owners = vec!["owner-id".to_owned()];
        assert!(proactive_target_allowed(
            &Scene::Private,
            "owner-id",
            "owner-id",
            &owners
        ));
        assert!(proactive_target_allowed(
            &Scene::GuildDm,
            "guild-id",
            "owner-id",
            &owners
        ));
        assert!(!proactive_target_allowed(
            &Scene::Group,
            "owner-id",
            "owner-id",
            &owners
        ));
        assert!(!proactive_target_allowed(
            &Scene::Private,
            "other-id",
            "other-id",
            &owners
        ));
    }
}
