//! 工具执行：模型要什么，端侧执行什么（ADR-05）。
//!
//! 边界（ADR-01 / P4）：
//! - 只有**注册过的能力**能被调用，没有 shell、没有任意命令；
//! - 服务端**永不执行**工具，只转发（工具执行永远在端侧）。

use std::collections::HashMap;

use crate::error::{CoreError, CoreResult};

/// 模型发起的一次工具调用。
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

/// 执行结果（会作为 `tool` 消息回填给模型）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolOutcome {
    pub ok: bool,
    pub content: String,
}

impl ToolOutcome {
    pub fn success(content: impl Into<String>) -> Self {
        Self {
            ok: true,
            content: content.into(),
        }
    }

    /// 失败也要回填给模型（让它自己改主意），而不是直接抛给用户。
    pub fn failure(reason: impl Into<String>) -> Self {
        Self {
            ok: false,
            content: reason.into(),
        }
    }
}

/// 工具处理器。同步即可：端侧数据都在本地。
pub type Handler = Box<dyn Fn(&serde_json::Value) -> CoreResult<String> + Send + Sync>;

/// 能力白名单：只有注册进来的名字可以被调用。
#[derive(Default)]
pub struct ToolRegistry {
    handlers: HashMap<String, Handler>,
}

impl std::fmt::Debug for ToolRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolRegistry")
            .field("tools", &self.handlers.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register<F>(&mut self, name: &str, handler: F)
    where
        F: Fn(&serde_json::Value) -> CoreResult<String> + Send + Sync + 'static,
    {
        self.handlers.insert(name.to_owned(), Box::new(handler));
    }

    pub fn names(&self) -> Vec<&str> {
        let mut v: Vec<&str> = self.handlers.keys().map(String::as_str).collect();
        v.sort_unstable();
        v
    }

    pub fn contains(&self, name: &str) -> bool {
        self.handlers.contains_key(name)
    }

    /// 执行一次调用。未知工具返回 `ok=false`——让模型知道"这个能力不存在"，
    /// 而不是让整个 turn 崩掉。
    pub fn execute(&self, call: &ToolCall) -> ToolOutcome {
        match self.handlers.get(&call.name) {
            None => ToolOutcome::failure(format!("未知工具：{}", call.name)),
            Some(handler) => match handler(&call.arguments) {
                Ok(content) => ToolOutcome::success(content),
                Err(e) => ToolOutcome::failure(format!("工具执行失败：{e}")),
            },
        }
    }
}

impl From<CoreError> for ToolOutcome {
    fn from(e: CoreError) -> Self {
        ToolOutcome::failure(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn registry() -> ToolRegistry {
        let mut r = ToolRegistry::new();
        r.register("get_courses", |args| {
            let weekday = args.get("weekday").and_then(|v| v.as_u64()).unwrap_or(0);
            Ok(format!("周{weekday}：无课"))
        });
        r.register("boom", |_| Err(CoreError::Invalid("参数缺失".into())));
        r
    }

    #[test]
    fn registered_tool_runs() {
        let r = registry();
        let out = r.execute(&ToolCall {
            id: "c1".into(),
            name: "get_courses".into(),
            arguments: json!({ "weekday": 3 }),
        });
        assert!(out.ok);
        assert!(out.content.contains("周3"));
    }

    #[test]
    fn unknown_tool_fails_softly() {
        let r = registry();
        let out = r.execute(&ToolCall {
            id: "c2".into(),
            name: "shell".into(), // 不存在的能力
            arguments: json!({}),
        });
        assert!(!out.ok);
        assert!(out.content.contains("未知工具"));
        assert!(!r.contains("shell"));
    }

    #[test]
    fn handler_error_becomes_tool_failure() {
        let r = registry();
        let out = r.execute(&ToolCall {
            id: "c3".into(),
            name: "boom".into(),
            arguments: json!({}),
        });
        assert!(!out.ok);
        assert!(out.content.contains("参数缺失"));
    }
}
