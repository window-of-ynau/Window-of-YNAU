# v0-legacy —— 最初的 Demo（历史留档）

这是「云农之窗」最初的原型：**FastAPI + 纯静态前端**，一个人用、双击就能跑。

> ⚠️ **它不是生产代码，也没有安全加固**。保留它是为了：
> 1. 留下产品最初的想法（节次换算规则、课表解析、画像设计都在这里萌芽）；
> 2. 给不写 Rust / Flutter 的贡献者一个可以上手的地方。

## 文件

```
main.py                 FastAPI 入口（13 个端点）
services/ai_service.py  模型调用、搜索、文档问答、识图
services/campus_api.py  校园链接、课表解析、周次/节次规则
services/memory_service.py  记忆/画像/文档状态的 JSON 持久化
static/                 单页前端（无框架、无构建）
parse_schedule_time.py  离线：教务课表 HTML → schedule_time.json
parse_schedule.py       更早的遗留脚本（已过时，仅留档）
start.bat               Windows 一键启动（uvicorn + cpolar 穿透）
```

## 怎么跑（仅本地实验）

```bash
python -m venv .venv && .venv\Scripts\activate      # Windows
pip install fastapi uvicorn openai requests beautifulsoup4 python-dotenv
copy .env.example .env                              # 然后填 DEEPSEEK_API_KEY / DASHSCOPE_API_KEY
uvicorn main:app --reload
```

（原仓库没有 `requirements.txt`，依赖以上面这条命令为准；欢迎补一个。）

## 已知问题（不要在生产复现）

安全隐患与设计缺陷已整理在：

- [`docs/v0-审计/03-接口文档.md`](../../docs/v0-审计/03-接口文档.md)：15 个端点逐个的问题清单
- [`docs/migration/v0-reuse.md`](../../docs/migration/v0-reuse.md)：哪些规则值得复用、哪些是坑

最关键的三条：

1. **零鉴权**：`/api/clear`、`/api/profile/reset`、`/api/profile` 全都裸奔，还有 CPolar 公网穿透；
2. **全局单例**：记忆/画像一份，第二个人访问就串号；
3. **`async def` 里做同步阻塞调用**：一个人提问，整站卡住。

主线（`crates/` + `app/`）已经把这些全部重做，所以这里**只修 bug、补文档、补测试**，不做架构改动。

## 可以做的事（欢迎认领 Issue）

- 补 `requirements.txt` 与最小可跑说明
- 给课表解析补 pytest（`parse_schedule_time.py` 的周次/节次规则有测试就好移植）
- 把 `services/campus_api.py` 里的节次/周次规则与主线 `crates/core/src/parse/` 对齐，列出差异