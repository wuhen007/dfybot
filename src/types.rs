use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    Onebot,
    Official,
    Panel,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Scene {
    Private,
    Group,
    Guild,
    GuildDm,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Attachment {
    pub kind: String,
    pub url: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub sticker: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub id: String,
    pub channel: Channel,
    pub scene: Scene,
    pub target: String,
    pub sender: String,
    #[serde(default)]
    pub sender_name: String,
    pub text: String,
    pub mentioned: bool,
    pub timestamp: i64,
    #[serde(default)]
    pub attachments: Vec<Attachment>,
    #[serde(default)]
    pub reply_to: Option<String>,
    #[serde(default)]
    pub proactive: bool,
}

impl Event {
    pub fn session(&self) -> String {
        // IDs remain opaque; official openids must never be equated to QQ numbers.
        serde_json::to_string(&(&self.channel, &self.scene, &self.target)).unwrap_or_default()
    }

    pub fn shard(&self, workers: usize) -> usize {
        let hash = Sha256::digest(self.session().as_bytes());
        u32::from_be_bytes([hash[0], hash[1], hash[2], hash[3]]) as usize % workers
    }

    pub fn is_private(&self) -> bool {
        matches!(self.scene, Scene::Private | Scene::GuildDm)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reply {
    pub text: String,
    #[serde(default)]
    pub voice: bool,
    #[serde(default)]
    pub sticker: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trace {
    pub session: String,
    pub kind: String,
    pub detail: String,
    pub timestamp: i64,
}

pub fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

pub fn truncate(s: &str, chars: usize) -> String {
    s.chars().take(chars).collect()
}

pub fn chunks(text: &str, max: usize, count: usize) -> Vec<String> {
    let chars: Vec<_> = text.chars().collect();
    if chars.is_empty() {
        return vec![];
    }
    chars
        .chunks(max.max(1))
        .take(count)
        .map(|s| s.iter().collect())
        .collect()
}
