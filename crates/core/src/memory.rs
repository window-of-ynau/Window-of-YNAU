//! 本地记忆（隐私层）：会话消息、被剪枝的原文（可恢复）。
//!
//! 规则来自 ADR-02 / ADR-10：**这些数据永不上云**。
//! 真实实现（W2）是 SQLite（WAL）；这里先给 trait 与内存实现，保证逻辑可测。

use std::collections::HashMap;

use sha2::{Digest, Sha256};
use time::OffsetDateTime;

use crate::error::{CoreError, CoreResult};

/// 消息角色（用字符串，避免把 shared 的类型绑死在存储里）。
pub const ROLE_USER: &str = "user";
pub const ROLE_ASSISTANT: &str = "assistant";
pub const ROLE_TOOL: &str = "tool";

/// 一条消息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub id: i64,
    pub session_id: String,
    pub role: String,
    pub content: String,
    pub tokens: u32,
    /// true 表示正文已移入 `memory_blobs`，这里只剩摘要或占位
    pub pruned: bool,
    pub created_at_ms: i64,
}

/// 被剪枝内容的种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlobKind {
    ToolOutput,
    DocChunk,
    EarlyTurn,
}

impl BlobKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ToolOutput => "tool_output",
            Self::DocChunk => "doc_chunk",
            Self::EarlyTurn => "early_turn",
        }
    }
}

/// 剪枝/压缩后的**原文**。模型可经 `memory://<id>` 指针取回。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Blob {
    pub id: String,
    pub session_id: String,
    pub message_id: Option<i64>,
    pub kind: BlobKind,
    pub content: String,
    pub sha256: String,
    pub created_at_ms: i64,
}

/// 记忆存储接口。
pub trait MemoryStore {
    fn append(
        &mut self,
        session_id: &str,
        role: &str,
        content: &str,
        tokens: u32,
    ) -> CoreResult<i64>;
    /// 最近 `limit` 条（按 id 升序返回）。
    fn recent(&self, session_id: &str, limit: usize) -> CoreResult<Vec<Message>>;
    /// 把一条消息的正文移入 blob，返回 blob id（幂等：已剪枝则返回已有指针）。
    fn prune(&mut self, message_id: i64, kind: BlobKind) -> CoreResult<String>;
    fn fetch_blob(&self, blob_id: &str) -> CoreResult<Blob>;
    fn delete_session(&mut self, session_id: &str) -> CoreResult<usize>;
    fn stats(&self) -> CoreResult<Stats>;
}

/// 记忆规模统计（用于上下文预算与 UI 展示）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Stats {
    pub messages: usize,
    pub blobs: usize,
    pub total_tokens: u32,
}

fn now_ms() -> i64 {
    OffsetDateTime::now_utc().unix_timestamp() * 1_000
}

fn sha256_hex(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// 内存实现（测试/开发用）。
#[derive(Debug, Default)]
pub struct InMemoryStore {
    messages: Vec<Message>,
    next_id: i64,
    blobs: HashMap<String, Blob>,
}

impl InMemoryStore {
    pub fn new() -> Self {
        Self {
            messages: Vec::new(),
            next_id: 1,
            blobs: HashMap::new(),
        }
    }
}

impl MemoryStore for InMemoryStore {
    fn append(
        &mut self,
        session_id: &str,
        role: &str,
        content: &str,
        tokens: u32,
    ) -> CoreResult<i64> {
        if session_id.trim().is_empty() {
            return Err(CoreError::Invalid("session_id 不能为空".into()));
        }
        let id = self.next_id;
        self.next_id += 1;
        self.messages.push(Message {
            id,
            session_id: session_id.to_owned(),
            role: role.to_owned(),
            content: content.to_owned(),
            tokens,
            pruned: false,
            created_at_ms: now_ms(),
        });
        Ok(id)
    }

    fn recent(&self, session_id: &str, limit: usize) -> CoreResult<Vec<Message>> {
        let mut out: Vec<Message> = self
            .messages
            .iter()
            .filter(|m| m.session_id == session_id)
            .cloned()
            .collect();
        out.sort_by_key(|m| m.id);
        if out.len() > limit {
            out = out.split_off(out.len() - limit);
        }
        Ok(out)
    }

    fn prune(&mut self, message_id: i64, kind: BlobKind) -> CoreResult<String> {
        let msg = self
            .messages
            .iter_mut()
            .find(|m| m.id == message_id)
            .ok_or_else(|| CoreError::Storage(format!("消息 {message_id} 不存在")))?;

        if msg.pruned {
            // 幂等：返回已有指针
            if let Some(b) = self
                .blobs
                .values()
                .find(|b| b.message_id == Some(message_id))
            {
                return Ok(b.id.clone());
            }
        }

        let blob_id = format!(
            "b_{}",
            sha256_hex(&format!("{}:{}", msg.id, msg.content))[..12].to_owned()
        );
        let blob = Blob {
            id: blob_id.clone(),
            session_id: msg.session_id.clone(),
            message_id: Some(msg.id),
            kind,
            content: msg.content.clone(),
            sha256: sha256_hex(&msg.content),
            created_at_ms: now_ms(),
        };
        // 剪枝后正文只留指针，token 计为 0
        msg.content = format!("[已剪枝 {kind} → memory://{blob_id}]", kind = kind.as_str());
        msg.pruned = true;
        msg.tokens = 0;
        self.blobs.insert(blob_id.clone(), blob);
        Ok(blob_id)
    }

    fn fetch_blob(&self, blob_id: &str) -> CoreResult<Blob> {
        self.blobs
            .get(blob_id)
            .cloned()
            .ok_or_else(|| CoreError::Storage(format!("blob {blob_id} 不存在")))
    }

    fn delete_session(&mut self, session_id: &str) -> CoreResult<usize> {
        let before = self.messages.len();
        self.messages.retain(|m| m.session_id != session_id);
        self.blobs.retain(|_, b| b.session_id != session_id);
        Ok(before - self.messages.len())
    }

    fn stats(&self) -> CoreResult<Stats> {
        Ok(Stats {
            messages: self.messages.len(),
            blobs: self.blobs.len(),
            total_tokens: self.messages.iter().map(|m| m.tokens).sum(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_and_recent_are_ordered_and_limited() {
        let mut s = InMemoryStore::new();
        for i in 1..=5 {
            s.append("s1", ROLE_USER, &format!("m{i}"), 3).unwrap();
        }
        s.append("s2", ROLE_USER, "other", 1).unwrap();
        let recent = s.recent("s1", 3).unwrap();
        assert_eq!(recent.len(), 3);
        assert_eq!(recent[2].content, "m5");
        assert!(recent.windows(2).all(|w| w[0].id < w[1].id));
        // 会话隔离
        assert_eq!(s.recent("s2", 10).unwrap().len(), 1);
    }

    #[test]
    fn prune_moves_content_and_is_idempotent() {
        let mut s = InMemoryStore::new();
        let id = s
            .append("s1", ROLE_TOOL, "很长很长的工具输出", 100)
            .unwrap();
        let blob_id = s.prune(id, BlobKind::ToolOutput).unwrap();
        let blob = s.fetch_blob(&blob_id).unwrap();
        assert_eq!(blob.content, "很长很长的工具输出");
        // 原文可恢复
        assert_eq!(blob.message_id, Some(id));
        // 幂等
        assert_eq!(s.prune(id, BlobKind::ToolOutput).unwrap(), blob_id);
        assert_eq!(s.stats().unwrap().blobs, 1);
        // 剪枝后 token 归零
        let msg = &s.recent("s1", 1).unwrap()[0];
        assert!(msg.pruned);
        assert_eq!(msg.tokens, 0);
        assert!(msg.content.contains("memory://"));
    }

    #[test]
    fn delete_session_removes_blobs_too() {
        let mut s = InMemoryStore::new();
        let id = s.append("s1", ROLE_TOOL, "x", 1).unwrap();
        let _ = s.prune(id, BlobKind::EarlyTurn).unwrap();
        assert_eq!(s.delete_session("s1").unwrap(), 1);
        assert_eq!(s.stats().unwrap().blobs, 0);
    }
}
