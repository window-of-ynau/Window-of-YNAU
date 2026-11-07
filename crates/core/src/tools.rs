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

/// 内置工具的 **wire 定义**（广告给上游的 `tools[]`）。
///
/// 没有这一步，模型根本不知道"端侧有课表/记忆可查"——它只会回"我没有你的课表上下文"
/// （t113 的真实事故）。`description` 用**中文**写清楚"什么时候该调"，
/// 因为用户是中文提问，模型按语义匹配。
pub fn builtin_specs() -> Vec<yunong_shared::ToolSpec> {
    use serde_json::json;

    vec![
        yunong_shared::ToolSpec {
            name: "timetable_query".to_owned(),
            description: "查询云农之窗本地课表：返回本学期各门课的星期、节次、地点与周次。\
                          用户问“哪天有课 / 哪节课最累 / 第 N 周上什么 / 某门课在哪上”时调用。"
                .to_owned(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "week": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 30,
                        "description": "第几周（从 1 开始）；不传则返回整学期"
                    }
                },
                "required": []
            }),
        },
        yunong_shared::ToolSpec {
            name: "memory_search".to_owned(),
            description: "在本地记忆与压缩存档里按关键词检索（例如“雨伞放哪了”“宿舍楼几栋”）。\
                          用户问起以前记过的事情、而当前上下文里没有时调用。"
                .to_owned(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "关键词，例如“雨伞”"
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 20,
                        "description": "最多返回几条（默认 5）"
                    }
                },
                "required": ["query"]
            }),
        },
    ]
}

/// 只保留**实际注册了处理器**的工具定义。
///
/// 为什么：广告出去但没注册的工具有害无益——模型会调它，然后拿到 `NOT_ALLOWED`
/// （用户看到一张失败卡片）。生产路径 `builtin_registry` 与 `builtin_specs` 一致；
/// 但测试/联调会传自定义注册表，这里按名字过滤，做到"广告的 = 能执行的"。
pub fn specs_for(names: &[String]) -> Vec<yunong_shared::ToolSpec> {
    builtin_specs()
        .into_iter()
        .filter(|spec| names.iter().any(|name| name == &spec.name))
        .collect()
}

/// 端侧内置工具：对话流里模型可以调用的**真实**能力。
///
/// 目前只有两个，因为"能改数据"的工具要非常克制（写错了用户就得手动收拾）：
///
/// | 名字 | 作用 | 参数 |
/// |---|---|---|
/// | `memory_search` | 搜本地记忆/压缩存档 | `{ "query": "雨伞", "limit": 5 }` |
/// | `timetable_query` | 查课表 | `{ "week": 8 }`（缺省 = 当前周） |
///
/// `db_path` 为空时返回一个**空注册表**——调用方据此走"不带工具"的普通流
/// （老行为完全不变）。工具返回值是**给人看的一句话 + 结论**，大 JSON 不进对话流
/// （见 [`crate::tool_events`] 的摘要约束）。
pub fn builtin_registry(db_path: Option<String>) -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    let Some(db_path) = db_path.filter(|path| !path.trim().is_empty()) else {
        return registry;
    };

    // 1) 本地记忆检索（复用 BM25 索引）
    let db_for_memory = db_path.clone();
    registry.register("memory_search", move |args| {
        let query = args
            .get("query")
            .and_then(|value| value.as_str())
            .ok_or_else(|| CoreError::Invalid("memory_search 需要 query".into()))?;
        let limit = args
            .get("limit")
            .and_then(|value| value.as_u64())
            .unwrap_or(5)
            .clamp(1, 20) as usize;
        let store = crate::local_search::SearchStore::open(&db_for_memory)?;
        let hits = store.search(query, limit)?;
        if hits.is_empty() {
            return Ok(format!("没搜到「{query}」相关的本地记忆"));
        }
        let mut out = format!("「{query}」找到 {} 条本地记忆：\n", hits.len());
        for hit in hits.iter().take(limit) {
            let title = hit.title.as_deref().unwrap_or("（无标题）");
            out.push_str(&format!("- {title}：{}\n", hit.snippet));
        }
        Ok(out)
    });

    // 2) 课表查询（读同一个库里的 courses 表）
    let db_for_schedule = db_path;
    registry.register("timetable_query", move |args| {
        let week_hint = args.get("week").and_then(|value| value.as_u64());
        let store = crate::store::CourseStore::open(&db_for_schedule)?;
        let terms = store.list_terms()?;
        let Some(term) = terms.first().map(|record| record.term.clone()) else {
            return Ok("还没有导入课表：先在课表页导入".to_owned());
        };
        let courses = store.list_courses(&term)?;
        if courses.is_empty() {
            return Ok(format!("学期 {term} 还没有课程数据"));
        }
        let mut out = format!("学期 {term} 共 {} 门课", courses.len());
        if let Some(week) = week_hint {
            out.push_str(&format!("（想看第 {week} 周，目前按全学期列出）"));
        }
        out.push_str("：\n");
        for course in courses.iter().take(20) {
            let room = course.room.as_deref().unwrap_or("地点待定");
            out.push_str(&format!(
                "- 周{} 第 {}-{} 节 {}（{}），周次 {}",
                course.weekday,
                course.start_period,
                course.end_period,
                course.name,
                room,
                course.weeks
            ));
            out.push('\n');
        }
        if courses.len() > 20 {
            out.push_str(&format!("（还有 {} 门未列出）\n", courses.len() - 20));
        }
        Ok(out)
    });
    registry
}

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
