# experiments/ —— 实验区

这里放**不参与生产构建**的实验代码，是给不写 Rust / Flutter 的贡献者（包括 v0 Demo 原作者）准备的。

## 与主线的关系

```
main ──┬──────────────────────────────►  生产代码（crates/ app/）
       │
       └── EXP ──► experiments/ 长期实验分支（会定期把 main 合进来）
```

- `EXP` 是从 `main` 分出来的**长期分支**，不是孤立历史；所以 `git merge main` 随时可用，不冲突。
- **CI 不构建 `experiments/`**：实验把主线弄红的概率为零。
- 成熟实验由维护者评审后「提升」为主线代码（Python 逻辑可保留为服务，或重写为 Rust）。

## 目录约定

| 路径 | 内容 | 是否可以随意改 |
|---|---|---|
| `experiments/v0-legacy/` | **历史留档**：最初的 FastAPI + 静态前端 Demo | 可以修 bug、补文档、补测试，但**不要**把它当生产代码 |
| `experiments/<你的特性>/` | 你自己的实验 | 随便改，带上 README |

## 硬性要求（只有三条）

1. **不放凭据**：Cookie、API Key、手机号、学号、个人绝对路径（`C:\Users\你的名字\...`）一律不提交；
2. **自带 README**：怎么跑、依赖什么、当前能做什么 / 不能做什么；
3. **依赖写清楚**：有第三方依赖就加 `requirements.txt`。

## 怎么开始（Python only）

```bash
git switch EXP
git merge main                     # 同步主线（保持自己的分支不落后）
mkdir -p experiments/你的特性
# 写代码 + README
git add -A
git commit -m "exp: 一句话说明你做了什么"
git push origin EXP
```

需要人 review 时，在 PR 里把 base 指到 `EXP`，标题加 `[EXP]`。
