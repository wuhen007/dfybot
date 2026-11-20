use crate::{
    config::Permission,
    types::{Event, Trace, now, truncate},
};
use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    path::Path,
    sync::{Arc, Mutex},
};

#[derive(Clone)]
pub struct Store(Arc<Mutex<Connection>>);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    pub content: String,
    pub sender: String,
    pub timestamp: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Memory {
    pub id: i64,
    pub session: String,
    pub kind: MemoryKind,
    pub subject: String,
    pub text: String,
    pub importance: u8,
    pub updated_at: i64,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemoryKind {
    Fact,
    Profile,
    Experience,
    Style,
}

impl MemoryKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fact => "fact",
            Self::Profile => "profile",
            Self::Experience => "experience",
            Self::Style => "style",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryDraft {
    pub session: String,
    pub kind: MemoryKind,
    pub subject: String,
    pub text: String,
    pub importance: u8,
}
#[derive(Debug, Clone, Serialize)]
pub struct Session {
    pub key: String,
    pub channel: String,
    pub scene: String,
    pub target: String,
    pub updated_at: i64,
    pub messages: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    pub enabled: bool,
    pub dry_run: bool,
    pub persona: String,
    pub group_mode: String,
    pub voice_replies: bool,
    pub proactive: bool,
    pub context_messages: usize,
    pub awake_start: u32,
    pub awake_end: u32,
    pub utc_offset_hours: i32,
    pub proactive_interval_minutes: i64,
    pub proactive_sessions: Vec<String>,
    pub group_keywords: Vec<String>,
    pub auto_memory: bool,
    pub memory_limit: usize,
    pub memory_recall: usize,
    pub cooldown_secs: i64,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let db = Connection::open(path).context("open SQLite database")?;
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA foreign_keys=ON;
        CREATE TABLE IF NOT EXISTS messages(id INTEGER PRIMARY KEY,session TEXT NOT NULL,role TEXT NOT NULL,sender TEXT NOT NULL,content TEXT NOT NULL,created_at INTEGER NOT NULL);
        CREATE INDEX IF NOT EXISTS messages_session ON messages(session,id DESC);
        CREATE TABLE IF NOT EXISTS memories(id INTEGER PRIMARY KEY,session TEXT NOT NULL,kind TEXT NOT NULL DEFAULT 'fact',subject TEXT NOT NULL,text TEXT NOT NULL,importance INTEGER NOT NULL DEFAULT 2,updated_at INTEGER NOT NULL, UNIQUE(session,subject,text));
        CREATE INDEX IF NOT EXISTS memories_session ON memories(session,updated_at DESC);
        CREATE TABLE IF NOT EXISTS traces(id INTEGER PRIMARY KEY,session TEXT NOT NULL,kind TEXT NOT NULL,detail TEXT NOT NULL,created_at INTEGER NOT NULL);
        CREATE INDEX IF NOT EXISTS traces_created ON traces(created_at DESC);
        CREATE TABLE IF NOT EXISTS stickers(id INTEGER PRIMARY KEY,source TEXT NOT NULL,hash TEXT NOT NULL UNIQUE,url TEXT NOT NULL,name TEXT NOT NULL DEFAULT '',tags TEXT NOT NULL DEFAULT '',uses INTEGER NOT NULL DEFAULT 0,created_at INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS settings(id INTEGER PRIMARY KEY CHECK(id=1),json TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS personas(name TEXT PRIMARY KEY,content TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS jobs(id INTEGER PRIMARY KEY,kind TEXT NOT NULL,session TEXT NOT NULL,due_at INTEGER NOT NULL,payload TEXT NOT NULL,enabled INTEGER NOT NULL DEFAULT 1,last_run INTEGER);
        CREATE INDEX IF NOT EXISTS jobs_due ON jobs(enabled,due_at);
        CREATE TABLE IF NOT EXISTS session_policy(session TEXT PRIMARY KEY,persona TEXT,model TEXT,mode TEXT,updated_at INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS received_events(event_key TEXT PRIMARY KEY,created_at INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS moods(session TEXT PRIMARY KEY,value REAL NOT NULL,updated_at INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS rate_limits(session TEXT NOT NULL,sender TEXT NOT NULL,at INTEGER NOT NULL,PRIMARY KEY(session,sender));")?;
        if !has_column(&db, "memories", "kind")? {
            db.execute(
                "ALTER TABLE memories ADD COLUMN kind TEXT NOT NULL DEFAULT 'fact'",
                [],
            )?;
        }
        Ok(Self(Arc::new(Mutex::new(db))))
    }
    pub fn backup(&self) -> Result<Vec<u8>> {
        let source = self.db();
        let mut target = Connection::open_in_memory()?;
        rusqlite::backup::Backup::new(&source, &mut target)?.run_to_completion(
            64,
            std::time::Duration::from_millis(20),
            None,
        )?;
        Ok(target.serialize(rusqlite::DatabaseName::Main)?.to_vec())
    }
    pub fn restore(&self, path: &Path) -> Result<()> {
        let source = Connection::open(path)?;
        let mut target = self.db();
        rusqlite::backup::Backup::new(&source, &mut target)?.run_to_completion(
            64,
            std::time::Duration::from_millis(20),
            None,
        )?;
        if !has_column(&target, "memories", "kind")? {
            target.execute(
                "ALTER TABLE memories ADD COLUMN kind TEXT NOT NULL DEFAULT 'fact'",
                [],
            )?;
        }
        Ok(())
    }
    fn db(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.0.lock().unwrap_or_else(|p| p.into_inner())
    }
    pub fn record(&self, e: &Event, user: &str, assistant: &str, context: usize) -> Result<()> {
        let mut db = self.db();
        let tx = db.transaction()?;
        let key = e.session();
        let at = now();
        tx.execute("INSERT INTO messages(session,role,sender,content,created_at) VALUES (?1,'user',?2,?3,?4)",params![key,e.sender,truncate(user,8000),at])?;
        tx.execute("INSERT INTO messages(session,role,sender,content,created_at) VALUES (?1,'assistant','bot',?2,?3)",params![key,truncate(assistant,8000),at])?;
        tx.execute("DELETE FROM messages WHERE session=?1 AND id NOT IN (SELECT id FROM messages WHERE session=?1 ORDER BY id DESC LIMIT ?2)",params![key,(context.max(2)*2) as i64])?;
        tx.commit()?;
        Ok(())
    }
    pub fn history(&self, e: &Event, limit: usize) -> Result<Vec<Message>> {
        let db = self.db();
        let mut stmt=db.prepare("SELECT role,sender,content,created_at FROM (SELECT role,sender,content,created_at,id FROM messages WHERE session=?1 ORDER BY id DESC LIMIT ?2) ORDER BY id")?;
        Ok(stmt
            .query_map(params![e.session(), limit.clamp(1, 100)], |r| {
                Ok(Message {
                    role: r.get(0)?,
                    sender: r.get(1)?,
                    content: r.get(2)?,
                    timestamp: r.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?)
    }
    pub fn add_memory(
        &self,
        session: &str,
        subject: &str,
        text: &str,
        importance: u8,
        limit: usize,
    ) -> Result<()> {
        self.add_typed_memory(session, MemoryKind::Fact, subject, text, importance, limit)
    }
    pub fn add_typed_memory(
        &self,
        session: &str,
        kind: MemoryKind,
        subject: &str,
        text: &str,
        importance: u8,
        limit: usize,
    ) -> Result<()> {
        let draft = MemoryDraft {
            session: session.into(),
            kind,
            subject: subject.into(),
            text: text.into(),
            importance,
        };
        self.add_memories(std::slice::from_ref(&draft), limit)
    }
    pub fn add_memories(&self, drafts: &[MemoryDraft], limit: usize) -> Result<()> {
        let mut db = self.db();
        let tx = db.transaction()?;
        let mut sessions = HashSet::new();
        for draft in drafts {
            tx.execute("INSERT INTO memories(session,kind,subject,text,importance,updated_at) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(session,subject,text) DO UPDATE SET kind=excluded.kind,importance=max(importance,excluded.importance),updated_at=excluded.updated_at",params![draft.session,draft.kind.as_str(),truncate(&draft.subject,100),truncate(&draft.text,1000),draft.importance.min(5),now()])?;
            sessions.insert(draft.session.as_str());
        }
        for session in sessions {
            tx.execute("DELETE FROM memories WHERE session=?1 AND id NOT IN (SELECT id FROM memories WHERE session=?1 ORDER BY importance DESC,updated_at DESC LIMIT ?2)",params![session,limit.max(1) as i64])?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn recall(
        &self,
        e: &Event,
        query: &str,
        limit: usize,
        include_shared: bool,
    ) -> Result<Vec<Memory>> {
        let db = self.db();
        let shared = if include_shared { "*" } else { "" };
        let mut stmt=db.prepare("SELECT id,session,kind,subject,text,importance,updated_at FROM memories WHERE session=?1 OR session=?2 ORDER BY importance DESC,updated_at DESC LIMIT ?3")?;
        let rows = stmt
            .query_map(
                params![e.session(), shared, limit.clamp(1, 20) as i64],
                |r| {
                    Ok(Memory {
                        id: r.get(0)?,
                        session: r.get(1)?,
                        kind: memory_kind(&r.get::<_, String>(2)?),
                        subject: r.get(3)?,
                        text: r.get(4)?,
                        importance: r.get(5)?,
                        updated_at: r.get(6)?,
                    })
                },
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let terms: Vec<String> = query
            .split(|c: char| !c.is_alphanumeric() && !('一'..='鿿').contains(&c))
            .filter(|s| !s.is_empty())
            .map(|s| s.to_lowercase())
            .collect();
        let mut scored: Vec<_> = rows
            .into_iter()
            .map(|m| {
                let hay = format!("{}{}", m.subject, m.text).to_lowercase();
                let score = terms.iter().map(|t| hay.matches(t).count()).sum::<usize>() * 10
                    + usize::from(m.importance) * 2
                    + (now() - m.updated_at)
                        .max(0)
                        .checked_div(86400)
                        .map(|d| 1000 / (d + 10) as usize)
                        .unwrap_or(0);
                (score, m)
            })
            .collect();
        scored.sort_by_key(|x| std::cmp::Reverse(x.0));
        Ok(scored
            .into_iter()
            .filter(|x| x.0 > 0)
            .take(limit.min(20))
            .map(|x| x.1)
            .collect())
    }
    pub fn memories(
        &self,
        session: &str,
        query: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<Memory>> {
        let db = self.db();
        let mut q=db.prepare("SELECT id,session,kind,subject,text,importance,updated_at FROM memories WHERE (session=?1 OR ?1='*') AND (?2='' OR instr(lower(subject||' '||text),lower(?2))>0) ORDER BY importance DESC,updated_at DESC LIMIT ?3 OFFSET ?4")?;
        Ok(q.query_map(
            params![
                session,
                truncate(query, 100),
                limit.clamp(1, 500) as i64,
                offset as i64
            ],
            |r| {
                Ok(Memory {
                    id: r.get(0)?,
                    session: r.get(1)?,
                    kind: memory_kind(&r.get::<_, String>(2)?),
                    subject: r.get(3)?,
                    text: r.get(4)?,
                    importance: r.get(5)?,
                    updated_at: r.get(6)?,
                })
            },
        )?
        .collect::<rusqlite::Result<_>>()?)
    }
    pub fn update_memory(&self, id: i64, draft: &MemoryDraft) -> Result<bool> {
        Ok(self.db().execute("UPDATE memories SET kind=?2,subject=?3,text=?4,importance=?5,updated_at=?6 WHERE id=?1",params![id,draft.kind.as_str(),truncate(&draft.subject,100),truncate(&draft.text,1000),draft.importance.min(5),now()])?==1)
    }
    pub fn delete_memory(&self, id: i64) -> Result<bool> {
        Ok(self
            .db()
            .execute("DELETE FROM memories WHERE id=?1", [id])?
            == 1)
    }
    pub fn traces(&self, limit: usize) -> Result<Vec<Trace>> {
        let db = self.db();
        let mut q = db.prepare(
            "SELECT session,kind,detail,created_at FROM traces ORDER BY id DESC LIMIT ?1",
        )?;
        Ok(q.query_map([limit.clamp(1, 500) as i64], |r| {
            Ok(Trace {
                session: r.get(0)?,
                kind: r.get(1)?,
                detail: r.get(2)?,
                timestamp: r.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?)
    }
    pub fn trace(&self, session: &str, kind: &str, detail: &str) {
        let _ = self.db().execute(
            "INSERT INTO traces(session,kind,detail,created_at) VALUES(?1,?2,?3,?4)",
            params![session, kind, truncate(detail, 1000), now()],
        );
    }
    pub fn claim_event(&self, event: &Event) -> Result<bool> {
        let key = format!("{:?}:{}", event.channel, event.id);
        Ok(self.db().execute(
            "INSERT OR IGNORE INTO received_events(event_key,created_at) VALUES(?1,?2)",
            params![key, now()],
        )? == 1)
    }
    pub fn release_event(&self, event: &Event) {
        let key = format!("{:?}:{}", event.channel, event.id);
        let _ = self
            .db()
            .execute("DELETE FROM received_events WHERE event_key=?1", [key]);
    }
    pub fn mood(&self, session: &str) -> f32 {
        let db = self.db();
        let Ok((value, at)) = db.query_row(
            "SELECT value,updated_at FROM moods WHERE session=?1",
            [session],
            |r| Ok((r.get::<_, f32>(0)?, r.get::<_, i64>(1)?)),
        ) else {
            return 0.0;
        };
        let days = (now() - at).max(0) as f32 / 86400.0;
        (value * 0.5_f32.powf(days / 2.0)).clamp(-5.0, 5.0)
    }
    pub fn affect(&self, session: &str, signal: f32) {
        let value = (self.mood(session) + signal).clamp(-5.0, 5.0);
        let _=self.db().execute("INSERT INTO moods(session,value,updated_at) VALUES(?1,?2,?3) ON CONFLICT(session) DO UPDATE SET value=excluded.value,updated_at=excluded.updated_at",params![session,value,now()]);
    }
    pub fn claim_cooldown(&self, session: &str, sender: &str, seconds: i64) -> Result<bool> {
        Ok(self.db().execute("INSERT INTO rate_limits(session,sender,at) VALUES(?1,?2,?3) ON CONFLICT(session,sender) DO UPDATE SET at=excluded.at WHERE rate_limits.at<=?4",params![session,sender,now(),now()-seconds.max(0)])?==1)
    }
    pub fn sessions(&self, limit: usize) -> Result<Vec<Session>> {
        let db = self.db();
        let mut q=db.prepare("SELECT session,max(created_at),count(*) FROM messages GROUP BY session ORDER BY max(created_at) DESC LIMIT ?1")?;
        let rows = q.query_map([limit.clamp(1, 500) as i64], |r| {
            let key: String = r.get(0)?;
            let parts: Vec<String> = serde_json::from_str(&key).unwrap_or_default();
            Ok(Session {
                channel: parts.first().cloned().unwrap_or_default(),
                scene: parts.get(1).cloned().unwrap_or_default(),
                target: parts.get(2).cloned().unwrap_or_default(),
                key,
                updated_at: r.get(1)?,
                messages: r.get(2)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
    pub fn messages(&self, key: &str, limit: usize) -> Result<Vec<Message>> {
        let db = self.db();
        let mut q=db.prepare("SELECT role,sender,content,created_at FROM (SELECT role,sender,content,created_at,id FROM messages WHERE session=?1 ORDER BY id DESC LIMIT ?2) ORDER BY id")?;
        Ok(q.query_map(params![key, limit.clamp(1, 100) as i64], |r| {
            Ok(Message {
                role: r.get(0)?,
                sender: r.get(1)?,
                content: r.get(2)?,
                timestamp: r.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?)
    }
    pub fn set_settings(&self, s: &Settings) -> Result<()> {
        self.db().execute("INSERT INTO settings(id,json) VALUES(1,?1) ON CONFLICT(id) DO UPDATE SET json=excluded.json",[serde_json::to_string(s)?])?;
        Ok(())
    }
    pub fn settings(&self, default: Settings) -> Settings {
        self.db()
            .query_row("SELECT json FROM settings WHERE id=1", [], |r| {
                r.get::<_, String>(0)
            })
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or(default)
    }
    pub fn set_persona(&self, name: &str, content: &str) -> Result<()> {
        self.db().execute("INSERT INTO personas(name,content) VALUES(?1,?2) ON CONFLICT(name) DO UPDATE SET content=excluded.content",params![truncate(name,50),truncate(content,32000)])?;
        Ok(())
    }
    pub fn personas(
        &self,
        defaults: &std::collections::BTreeMap<String, String>,
    ) -> Result<std::collections::BTreeMap<String, String>> {
        let mut out = defaults.clone();
        let db = self.db();
        let mut q = db.prepare("SELECT name,content FROM personas")?;
        for row in q.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
            let (n, c) = row?;
            out.insert(n, c);
        }
        Ok(out)
    }
    pub fn set_session_persona(&self, key: &str, persona: Option<&str>) -> Result<()> {
        self.db().execute("INSERT INTO session_policy(session,persona,updated_at) VALUES(?1,?2,?3) ON CONFLICT(session) DO UPDATE SET persona=excluded.persona,updated_at=excluded.updated_at",params![key,persona,now()])?;
        Ok(())
    }
    pub fn session_persona(&self, key: &str) -> Option<String> {
        self.db()
            .query_row(
                "SELECT persona FROM session_policy WHERE session=?1",
                [key],
                |r| r.get(0),
            )
            .ok()
            .flatten()
    }
    pub fn session_parts(key: &str) -> Result<(String, String, String)> {
        let p: Vec<String> = serde_json::from_str(key)?;
        if p.len() != 3 {
            return Err(anyhow::anyhow!("invalid session key"));
        }
        Ok((p[0].clone(), p[1].clone(), p[2].clone()))
    }
    pub fn save_sticker(&self, source: &str, hash: &str, url: &str, name: &str) -> Result<bool> {
        Ok(self.db().execute("INSERT OR IGNORE INTO stickers(source,hash,url,name,created_at) VALUES(?1,?2,?3,?4,?5)",params![source,hash,url,truncate(name,100),now()])?==1)
    }
    pub fn sticker_matches(&self, query: &str, limit: usize) -> Result<Vec<serde_json::Value>> {
        let rows = self.stickers(100)?;
        let terms: Vec<_> = query
            .split(|c: char| !c.is_alphanumeric())
            .filter(|x| x.chars().count() > 1)
            .map(str::to_lowercase)
            .collect();
        let mut ranked: Vec<_> = rows
            .into_iter()
            .map(|x| {
                let hay = format!(
                    "{} {}",
                    x["name"].as_str().unwrap_or(""),
                    x["tags"].as_str().unwrap_or("")
                )
                .to_lowercase();
                let score = terms.iter().filter(|t| hay.contains(t.as_str())).count();
                (score, x)
            })
            .filter(|(score, _)| *score > 0)
            .collect();
        ranked
            .sort_by_key(|(score, x)| std::cmp::Reverse((*score, x["uses"].as_i64().unwrap_or(0))));
        Ok(ranked
            .into_iter()
            .take(limit.min(10))
            .map(|x| x.1)
            .collect())
    }
    pub fn use_sticker(&self, id: i64) -> Result<()> {
        self.db()
            .execute("UPDATE stickers SET uses=uses+1 WHERE id=?1", [id])?;
        Ok(())
    }
    pub fn stickers(&self, limit: usize) -> Result<Vec<serde_json::Value>> {
        let db = self.db();
        let mut q=db.prepare("SELECT id,name,tags,url,uses FROM stickers ORDER BY uses DESC,created_at DESC LIMIT ?1")?;
        Ok(q.query_map([limit.clamp(1,100) as i64],|r|Ok(serde_json::json!({"id":r.get::<_,i64>(0)?,"name":r.get::<_,String>(1)?,"tags":r.get::<_,String>(2)?,"url":r.get::<_,String>(3)?,"uses":r.get::<_,i64>(4)?})))?.collect::<rusqlite::Result<_>>()?)
    }
    pub fn delete_sticker(&self, id: i64) -> Result<bool> {
        Ok(self
            .db()
            .execute("DELETE FROM stickers WHERE id=?1", [id])?
            == 1)
    }
    pub fn sticker(&self, id: i64) -> Result<Option<(String, String, String)>> {
        Ok(self
            .db()
            .query_row(
                "SELECT source,url,name FROM stickers WHERE id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?)
    }
    pub fn update_sticker(&self, id: i64, name: &str, tags: &str) -> Result<bool> {
        Ok(self.db().execute(
            "UPDATE stickers SET name=?2,tags=?3 WHERE id=?1",
            params![id, truncate(name, 100), truncate(tags, 300)],
        )? == 1)
    }
    pub fn janitor(&self, days: i64, memory_limit: usize) -> Result<()> {
        let db = self.db();
        db.execute(
            "DELETE FROM messages WHERE created_at<?1",
            [now() - days.max(1) * 86400],
        )?;
        db.execute(
            "DELETE FROM traces WHERE created_at<?1",
            [now() - days.max(1) * 86400],
        )?;
        db.execute(
            "DELETE FROM received_events WHERE created_at<?1",
            [now() - days.max(1) * 86400],
        )?;
        db.execute(
            "DELETE FROM rate_limits WHERE at<?1",
            [now() - days.max(1) * 86400],
        )?;
        db.execute("DELETE FROM memories WHERE session!='*' AND id NOT IN (SELECT id FROM memories m2 WHERE m2.session=memories.session ORDER BY importance DESC,updated_at DESC LIMIT ?1)",[memory_limit.max(1) as i64])?;
        db.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
        Ok(())
    }
    pub fn forget_session(&self, key: &str) -> Result<()> {
        let db = self.db();
        db.execute("DELETE FROM messages WHERE session=?1", [key])?;
        db.execute("DELETE FROM memories WHERE session=?1", [key])?;
        db.execute("DELETE FROM traces WHERE session=?1", [key])?;
        db.execute("DELETE FROM session_policy WHERE session=?1", [key])?;
        Ok(())
    }
    pub fn permission_allowed(permission: Permission, owner: bool, private: bool) -> bool {
        match permission {
            Permission::Off => false,
            Permission::OwnerPrivate => owner && private,
            Permission::Everyone => true,
        }
    }
}

fn has_column(db: &Connection, table: &str, column: &str) -> Result<bool> {
    let mut stmt = db.prepare(&format!("PRAGMA table_info({table})"))?;
    for name in stmt.query_map([], |row| row.get::<_, String>(1))? {
        if name? == column {
            return Ok(true);
        }
    }
    Ok(false)
}

fn memory_kind(kind: &str) -> MemoryKind {
    match kind {
        "profile" => MemoryKind::Profile,
        "experience" => MemoryKind::Experience,
        "style" => MemoryKind::Style,
        _ => MemoryKind::Fact,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Channel, Scene};
    #[test]
    fn session_keys_partition_channels() {
        let e = Event {
            id: "1".into(),
            channel: Channel::Onebot,
            scene: Scene::Private,
            target: "123".into(),
            sender: "123".into(),
            sender_name: String::new(),
            text: "hi".into(),
            mentioned: false,
            timestamp: 0,
            attachments: vec![],
            reply_to: None,
            proactive: false,
        };
        let mut o = e.clone();
        o.channel = Channel::Official;
        assert_ne!(e.session(), o.session());
    }
    #[test]
    fn retention_is_bounded() {
        let s = Store::open(Path::new(":memory:")).unwrap();
        for i in 0..10 {
            s.add_memory("x", "fact", &format!("fact{i}"), 1, 3)
                .unwrap();
        }
        assert_eq!(s.memories("x", "", 100, 0).unwrap().len(), 3);
    }
    #[test]
    fn upgrades_old_memory_tables_without_losing_rows() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let db = Connection::open(file.path()).unwrap();
        db.execute_batch("CREATE TABLE memories(id INTEGER PRIMARY KEY,session TEXT NOT NULL,subject TEXT NOT NULL,text TEXT NOT NULL,importance INTEGER NOT NULL DEFAULT 2,updated_at INTEGER NOT NULL,UNIQUE(session,subject,text)); INSERT INTO memories(session,subject,text,importance,updated_at) VALUES('s','p','likes tea',2,1);")
            .unwrap();
        drop(db);
        let store = Store::open(file.path()).unwrap();
        let rows = store.memories("s", "tea", 10, 0).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].kind, MemoryKind::Fact);
    }
    #[test]
    fn restoring_an_old_database_migrates_memory_rows() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let old = Connection::open(file.path()).unwrap();
        old.execute_batch("CREATE TABLE memories(id INTEGER PRIMARY KEY,session TEXT NOT NULL,subject TEXT NOT NULL,text TEXT NOT NULL,importance INTEGER NOT NULL DEFAULT 2,updated_at INTEGER NOT NULL,UNIQUE(session,subject,text)); INSERT INTO memories(session,subject,text,importance,updated_at) VALUES('s','p','likes tea',2,1);")
            .unwrap();
        drop(old);
        let store = Store::open(Path::new(":memory:")).unwrap();
        store.restore(file.path()).unwrap();
        let rows = store.memories("s", "", 10, 0).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].kind, MemoryKind::Fact);
    }
    #[test]
    fn memory_categories_and_search_are_persisted() {
        let store = Store::open(Path::new(":memory:")).unwrap();
        store
            .add_typed_memory(
                "s",
                MemoryKind::Style,
                "owner",
                "likes short answers",
                3,
                10,
            )
            .unwrap();
        let rows = store.memories("s", "short", 10, 0).unwrap();
        assert_eq!(rows[0].kind, MemoryKind::Style);
        assert_eq!(store.memories("s", "absent", 10, 0).unwrap().len(), 0);
    }
    #[test]
    fn cooldown_is_per_sender_and_session() {
        let s = Store::open(Path::new(":memory:")).unwrap();
        assert!(s.claim_cooldown("a", "u", 2).unwrap());
        assert!(!s.claim_cooldown("a", "u", 2).unwrap());
        assert!(s.claim_cooldown("b", "u", 2).unwrap());
    }
}
