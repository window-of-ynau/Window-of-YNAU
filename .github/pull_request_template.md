## 变更说明

<!-- 做了什么、为什么这么做。一句话说清动机；细节放正文。 -->

- 
- 

## 关联 ADR 或 issue

<!-- 触及信任域 / 凭据 / 契约 / 许可证的改动必须先有 ADR，见 docs/04-ADR.md -->

- ADR：
- Issue：Closes #

## 验证方式

<!-- 写清怎么证明它可用，别只写"自测通过" -->

- [ ] `cargo test --workspace` 通过
- [ ] `cd app && flutter test` 通过
- [ ] 手动验证路径：
- [ ] benchmark 对比（如涉及模型调用）：`bench/results/xxx.json`

## 是否影响契约 `shared`

<!-- shared 被端与服务端同时消费，任何改动都是破坏性变更 -->

- [ ] 不影响 `crates/shared`
- [ ] 影响 `crates/shared`：已同步更新 `docs/03-接口文档.md`，并说明兼容策略（本次为破坏性变更，需同步升级端与服务端）
- [ ] 影响数据库 schema：新增迁移 `crates/server/migrations/xxxx.sql`，且可重复执行

## 自检清单

- [ ] 改动范围聚焦，未混入无关重构或格式化噪音
- [ ] 没有提交任何密钥、keystore、`key.properties`、`.env`、真实校务数据
- [ ] 没有让凭据（Cookie / Session / BYOK Key）离开端侧
- [ ] 日志未打印正文、Cookie、API Key、学号姓名明文
- [ ] 中文注释/文档已同步；破坏性改动已在描述中标注
- [ ] 本地跑过与 CI 相同的检查（`cargo fmt/clippy`、`flutter analyze`、`check_docs.py`）
- [ ] 目标分支正确（`dev` 用于日常集成分支，`main` 仅发布）
