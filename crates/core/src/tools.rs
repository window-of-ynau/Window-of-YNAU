//! 工具执行：模型要什么，端侧执行什么（ADR-05）。
//!
//! 边界（ADR-01 / P4）：
//! - 只有**注册过的能力**能被调用，没有 shell、没有任意命令；
//! - 服务端**永不执行**工具，只转发（工具执行永远在端侧）。

use std::collections::HashMap;
use std::sync::Arc;

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
    /// 这个工具说"到此为止"：整批工具都 `terminate` 时，工具循环不再发起下一轮
    /// （见 [`crate::tool_loop`]；对齐 pi `agent-loop.ts` 的 terminate 语义）。
    pub terminate: bool,
}

impl ToolOutcome {
    pub fn success(content: impl Into<String>) -> Self {
        Self {
            ok: true,
            content: content.into(),
            terminate: false,
        }
    }

    /// 失败也要回填给模型（让它自己改主意），而不是直接抛给用户。
    pub fn failure(reason: impl Into<String>) -> Self {
        Self {
            ok: false,
            content: reason.into(),
            terminate: false,
        }
    }

    /// 标记"这批工具执行完就收尾"（例如 `finish` / `final_answer` 这类终结型工具）。
    pub fn with_terminate(mut self) -> Self {
        self.terminate = true;
        self
    }
}

/// 工具处理器。同步即可：端侧数据都在本地。
///
/// 返回 [`ToolOutcome`] 而不是裸字符串：工具要能表达"成功/失败"与"到此为止"
/// （`terminate`，见 [`crate::tool_loop`]）。普通工具用
/// [`ToolRegistry::register`] 注册即可，它会把 `Result<String>` 包成成功结果。
///
/// 用 `Arc` 而不是 `Box`：工具循环要把它移进**带超时**的工作线程
/// （见 [`crate::tool_loop`]），`Arc<dyn Fn>` 能在不借用整个注册表的前提下克隆一份可调用句柄。
pub type Handler = Arc<dyn Fn(&serde_json::Value) -> CoreResult<ToolOutcome> + Send + Sync>;

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

    /// 注册一个"返回文本"的普通工具（失败用 `Err`）。
    pub fn register<F>(&mut self, name: &str, handler: F)
    where
        F: Fn(&serde_json::Value) -> CoreResult<String> + Send + Sync + 'static,
    {
        let handler: Handler = Arc::new(move |args| handler(args).map(ToolOutcome::success));
        self.handlers.insert(name.to_owned(), handler);
    }

    /// 注册一个能自己决定成败/收尾的工具（`terminate` 语义只有这条路径能表达）。
    pub fn register_outcome<F>(&mut self, name: &str, handler: F)
    where
        F: Fn(&serde_json::Value) -> CoreResult<ToolOutcome> + Send + Sync + 'static,
    {
        self.handlers.insert(name.to_owned(), Arc::new(handler));
    }

    pub fn names(&self) -> Vec<&str> {
        let mut v: Vec<&str> = self.handlers.keys().map(String::as_str).collect();
        v.sort_unstable();
        v
    }

    pub fn contains(&self, name: &str) -> bool {
        self.handlers.contains_key(name)
    }

    /// 取一份可调用的句柄（`Arc`，可移进工作线程）。
    ///
    /// 工具循环用它做"单工具超时"：把句柄移进线程，超时就丢掉结果
    /// （同步 handler 没有可中断的原语，见 [`crate::tool_loop`] 的说明）。
    pub fn handler(&self, name: &str) -> Option<Handler> {
        self.handlers.get(name).cloned()
    }

    /// 执行一次调用。未知工具返回 `ok=false`——让模型知道"这个能力不存在"，
    /// 而不是让整个 turn 崩掉。
    pub fn execute(&self, call: &ToolCall) -> ToolOutcome {
        match self.handler(&call.name) {
            None => ToolOutcome::failure(format!("未知工具：{}", call.name)),
            Some(handler) => match handler(&call.arguments) {
                Ok(outcome) => outcome,
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

    #[test]
    fn terminate_flag_defaults_off_and_is_opt_in() {
        assert!(!ToolOutcome::success("x").terminate);
        assert!(!ToolOutcome::failure("x").terminate);
        assert!(ToolOutcome::success("done").with_terminate().terminate);
        assert!(ToolOutcome::failure("stop").with_terminate().terminate);
    }

    #[test]
    fn register_outcome_can_mark_a_terminating_tool() {
        let mut r = ToolRegistry::new();
        r.register_outcome("finish", |_| {
            Ok(ToolOutcome::success("回答完毕").with_terminate())
        });
        let out = r.execute(&ToolCall {
            id: "c1".into(),
            name: "finish".into(),
            arguments: json!({}),
        });
        assert!(out.ok && out.terminate);

        // 普通 register 注册的工具默认不 terminate
        let out = r.execute(&ToolCall {
            id: "c2".into(),
            name: "get_courses".into(),
            arguments: json!({}),
        });
        assert!(!out.ok, "未注册 → 失败");
        assert!(!out.terminate);
    }

    #[test]
    fn handler_handle_can_be_cloned_and_called_outside_the_registry() {
        let r = registry();
        let handler = r.handler("get_courses").expect("已注册");
        // 句柄可以移进线程（工具循环的超时路径就是这么用的）
        let out = std::thread::spawn(move || handler(&json!({ "weekday": 5 })))
            .join()
            .expect("线程不应 panic")
            .expect("工具应成功");
        assert_eq!(out.content, "周5：无课");
        assert!(out.ok);
        assert!(r.handler("shell").is_none());
    }
}
