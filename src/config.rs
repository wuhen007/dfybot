use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::Path};

#[derive(Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub server: Server,
    pub chat: Chat,
    pub model: Model,
    pub onebot: Onebot,
    pub official: Official,
    pub voice: Voice,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Server {
    pub listen: String,
    pub data_dir: String,
    pub admin_token_env: String,
    pub workers: usize,
    pub queue_capacity: usize,
    pub retention_days: i64,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Chat {
    pub enabled: bool,
    pub dry_run: bool,
    pub owners: Vec<String>,
    pub allowed_sessions: Vec<String>,
    pub group_mode: String,
    pub group_keywords: Vec<String>,
    pub group_debounce_ms: u64,
    pub context_messages: usize,
    pub max_input_chars: usize,
    pub max_reply_chars: usize,
    pub max_reply_parts: usize,
    pub cooldown_secs: i64,
    pub memory_limit: usize,
    pub memory_recall: usize,
    pub auto_memory: bool,
    pub proactive: bool,
    pub proactive_interval_minutes: i64,
    pub proactive_sessions: Vec<String>,
    pub proactive_goals: Vec<String>,
    pub night_message: bool,
    pub night_message_hour: u32,
    pub awake_start: u32,
    pub awake_end: u32,
    pub utc_offset_hours: i32,
    pub personas: BTreeMap<String, String>,
    pub persona: String,
    pub session_personas: BTreeMap<String, String>,
    pub session_models: BTreeMap<String, String>,
    pub voice_replies: bool,
    pub stickers: bool,
    pub memory_permission: Permission,
}

#[derive(Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Permission {
    Off,
    OwnerPrivate,
    Everyone,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Model {
    pub base_url: String,
    pub api_key_env: String,
    pub name: String,
    pub timeout_secs: u64,
    pub max_tokens: u32,
    pub temperature: f32,
    pub vision: bool,
}
#[derive(Clone, Serialize, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct Onebot {
    pub enabled: bool,
    pub ws_url: String,
    pub http_url: String,
    pub token_env: String,
    pub bot_id: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Official {
    pub enabled: bool,
    pub transport: String,
    pub app_id: String,
    pub secret_env: String,
    pub base_url: String,
    pub token_url: String,
    pub reply_window_secs: i64,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Voice {
    pub enabled: bool,
    pub base_url: String,
    pub api_key_env: String,
    pub asr_model: String,
    pub tts_model: String,
    pub tts_voice: String,
    pub tts_format: String,
    pub max_bytes: usize,
    pub media_hosts: Vec<String>,
    pub ffmpeg: String,
    pub silk_decoder: String,
    pub silk_encoder: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            server: Server::default(),
            chat: Chat::default(),
            model: Model::default(),
            onebot: Onebot::default(),
            official: Official::default(),
            voice: Voice::default(),
        }
    }
}
impl Default for Server {
    fn default() -> Self {
        Self {
            listen: "127.0.0.1:8787".into(),
            data_dir: "data".into(),
            admin_token_env: "AURABOT_ADMIN_TOKEN".into(),
            workers: 2,
            queue_capacity: 256,
            retention_days: 30,
        }
    }
}
impl Default for Chat {
    fn default() -> Self {
        Self { enabled:false,dry_run:true,owners:vec![],allowed_sessions:vec![],group_mode:"mention".into(),group_keywords:vec![],group_debounce_ms:800,context_messages:24,max_input_chars:6000,max_reply_chars:1200,max_reply_parts:3,cooldown_secs:2,memory_limit:3000,memory_recall:6,auto_memory:true,proactive:false,proactive_interval_minutes:120,proactive_sessions:vec![],proactive_goals:vec!["问候主人，轻松开启一段对话".into(),"自然地接续最近聊过的话题".into(),"分享一个简短的问题或观察".into()],night_message:false,night_message_hour:22,awake_start:8,awake_end:23,utc_offset_hours:8,personas:BTreeMap::from([("default".into(),"你是大肥鱼，一个温暖、自然、有自己想法的聊天伙伴。尊重每个人的隐私，不冒充真人。用简洁的中文交流。".into())]),persona:"default".into(),session_personas:BTreeMap::new(),session_models:BTreeMap::new(),voice_replies:false,stickers:true,memory_permission:Permission::OwnerPrivate }
    }
}
impl Default for Model {
    fn default() -> Self {
        Self {
            base_url: "https://api.openai.com/v1".into(),
            api_key_env: "AURABOT_MODEL_KEY".into(),
            name: "".into(),
            timeout_secs: 60,
            max_tokens: 1200,
            temperature: 0.8,
            vision: false,
        }
    }
}
impl Default for Official {
    fn default() -> Self {
        Self {
            enabled: false,
            transport: "webhook".into(),
            app_id: "".into(),
            secret_env: "AURABOT_QQ_SECRET".into(),
            base_url: "https://api.sgroup.qq.com".into(),
            token_url: "https://bots.qq.com/app/getAppAccessToken".into(),
            reply_window_secs: 240,
        }
    }
}
impl Default for Voice {
    fn default() -> Self {
        Self {
            enabled: false,
            base_url: "https://api.openai.com/v1".into(),
            api_key_env: "AURABOT_VOICE_KEY".into(),
            asr_model: "whisper-1".into(),
            tts_model: "tts-1".into(),
            tts_voice: "alloy".into(),
            tts_format: "mp3".into(),
            max_bytes: 8 * 1024 * 1024,
            media_hosts: vec![],
            ffmpeg: "".into(),
            silk_decoder: "".into(),
            silk_encoder: "".into(),
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let value: Self =
            toml::from_str(&std::fs::read_to_string(path).context("read configuration")?)
                .context("parse configuration")?;
        value.validate()?;
        Ok(value)
    }
    pub fn validate(&self) -> Result<()> {
        let s = &self.server;
        let c = &self.chat;
        s.listen
            .parse::<std::net::SocketAddr>()
            .context("server.listen must be an IP:port")?;
        if !(1..=8).contains(&s.workers) || !(8..=10000).contains(&s.queue_capacity) {
            bail!("workers must be 1..8; queue_capacity 8..10000");
        }
        if s.retention_days < 1 || s.retention_days > 3650 {
            bail!("retention_days must be 1..3650");
        }
        if !(1..=100).contains(&c.context_messages)
            || !(1..=12).contains(&c.max_reply_parts)
            || !(1..=10000).contains(&c.max_reply_chars)
            || !(1..=32000).contains(&c.max_input_chars)
        {
            bail!("invalid context/input/reply bounds");
        }
        if !["mention", "keyword", "all"].contains(&c.group_mode.as_str())
            || c.group_debounce_ms > 3000
        {
            bail!("invalid group policy");
        }
        if !(1..=100000).contains(&c.memory_limit) || c.memory_recall > 20 || c.cooldown_secs < 0 {
            bail!("invalid memory/cooldown limits");
        }
        if !c.personas.contains_key(&c.persona)
            || c.personas.values().any(|v| v.len() > 32000)
            || c.session_personas
                .values()
                .any(|p| !c.personas.contains_key(p))
        {
            bail!("invalid persona selection or content");
        }
        if c.awake_start > 23
            || c.awake_end > 24
            || !(-12..=14).contains(&c.utc_offset_hours)
            || c.proactive_interval_minutes < 5
            || c.night_message_hour > 23
            || c.proactive_goals.len() > 12
            || c.proactive_goals
                .iter()
                .any(|g| g.trim().is_empty() || g.len() > 200)
        {
            bail!("invalid proactive schedule");
        }
        for url in [
            &self.model.base_url,
            &self.voice.base_url,
            &self.official.base_url,
            &self.official.token_url,
        ] {
            validate_url(url, &["https", "http"])?;
        }
        if !(5..=180).contains(&self.model.timeout_secs)
            || !(1..=8192).contains(&self.model.max_tokens)
            || !(0.0..=2.0).contains(&self.model.temperature)
        {
            bail!("invalid model bounds");
        }
        if self.onebot.enabled {
            validate_url(&self.onebot.ws_url, &["ws", "wss"])?;
            validate_url(&self.onebot.http_url, &["http", "https"])?;
            if self.onebot.bot_id.is_empty() {
                bail!("onebot.bot_id is required");
            }
        }
        if self.official.enabled
            && (self.official.app_id.is_empty()
                || !(30..=240).contains(&self.official.reply_window_secs)
                || !["webhook", "websocket"].contains(&self.official.transport.as_str()))
        {
            bail!("invalid official bot configuration");
        }
        if self.voice.max_bytes < 1024
            || self.voice.max_bytes > 25 * 1024 * 1024
            || !["mp3", "wav", "opus"].contains(&self.voice.tts_format.as_str())
        {
            bail!("invalid voice bounds/format");
        }
        if self.voice.enabled && self.onebot.enabled && self.voice.silk_encoder.is_empty() {
            bail!("voice.silk_encoder is required when OneBot voice replies are enabled")
        }
        Ok(())
    }
}

pub fn validate_url(value: &str, schemes: &[&str]) -> Result<()> {
    let url = reqwest::Url::parse(value)?;
    if !schemes.contains(&url.scheme())
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        bail!("invalid service URL");
    }
    Ok(())
}

pub fn secret(env: &str) -> Result<String> {
    let v = std::env::var(env).with_context(|| format!("missing environment variable {env}"))?;
    if v.len() < 8 {
        bail!("environment variable {env} is too short");
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_unbounded_and_invalid_configs() {
        let mut c = Config::default();
        assert!(c.validate().is_ok());
        c.server.workers = 0;
        assert!(c.validate().is_err());
        c.server.workers = 2;
        c.chat.persona = "missing".into();
        assert!(c.validate().is_err());
        assert!(toml::from_str::<Config>("[chat]\nmax_reply_part=3").is_err());
    }
}
