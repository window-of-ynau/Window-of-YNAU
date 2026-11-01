# W6 人工验收步骤（真实登录 + Cookie 复用）

> 自动化只能覆盖到 bridge/状态机；**真实登录必须人工点一遍**。
> 本文件就是那份"照着做就能判断成不成"的清单。

## 0. 前置

| 项 | 要求 |
|---|---|
| 系统 | Windows 10/11，已开**开发者模式**（Flutter 插件需要符号链接） |
| 运行时 | 已装 WebView2 Runtime（Win10 1803+ 一般自带；没有就装 Evergreen Runtime） |
| 工具链 | `nuget.exe` 在 `PATH`（`flutter_inappwebview` 的 Windows 实现要在构建期拉 `nlohmann.json`） |
| 网络 | 能访问 `qzjwxt.ynau.edu.cn`（校园网 / VPN） |
| 账号 | 一个真实学号 + 密码（**不要**贴进任何工单或聊天） |

构建（在该 worktree 的 `app/` 下）：

```powershell
$env:PATH = "$env:USERPROFILE\.nuget;$env:PATH"   # 或 nuget.exe 所在目录
cargo build --release -p yunong-core --manifest-path ..\Cargo.toml
Copy-Item ..\target\release\yunong_core.dll .\build\windows\x64\runner\Debug\ -Force
flutter --no-version-check build windows --debug
```

## 1. 首次登录（必须成功）

1. 启动 `build\windows\x64\runner\Debug\yunong.exe`；
2. 左侧进「我的课表」→ 点「登录并抓取课表」；
3. 应弹出全屏 WebView，停在**统一身份认证登录页**，顶部提示
   「请在校务系统里完成登录（登录成功后会自动读取课表）」；
4. 在 WebView 里输入学号/密码（含验证码）并登录；
5. **不要**再点任何按钮 —— 登录成功后顶部提示应自动变成
   「已登录，正在读取课表…」，随后页面自动关闭；
6. 课表列表出现课程，提示「已从校务系统抓取，本学期 N 门课」。

判定：5、6 两步自动发生 = **通过**；如果停在登录页不动、或要你再点"抓取当前页" = 不通过。

## 2. 会话复用（W6 的核心）

1. 完全退出 App（关窗口，确认任务管理器里没有 `yunong.exe`）；
2. 重新启动，进「我的课表」→ 点「登录并抓取课表」；
3. 应**直接**落到课表页（不出现登录表单），并在 2~10 秒内自动关闭、列表刷新。

判定：没让你重新输密码 = **通过**。

## 3. 会话存放位置核对（隐私）

- 打开 `%LOCALAPPDATA%\yunong\webview\`，应看到 WebView2 的配置文件（含 `Cookies` 文件）——
  **这就是会话唯一存放处**；
- 确认课表数据库 `%LOCALAPPDATA%\yunong\timetable.db` 里**没有**任何 Cookie/密码字段
  （用任意 SQLite 工具看 `terms` / `courses` 两表即可）；
- App 日志/控制台里搜 `cookie`、`sessionid`、`ticket`，应为 0 条。

## 4. 失败路径（三条都要看）

| 场景 | 怎么做 | 期望 |
|---|---|---|
| 登录超时 | 打开 WebView 后**不登录**，干等 5 分钟 | 关闭并提示「登录超时，请重试」，卡片上有「重试」按钮 |
| 页面改版 | 用「模拟页面改版」（调试按钮）导一份没有课表的 HTML | 提示「学校页面可能已更新，请反馈」，**列表里旧课表仍在**，数据库不被清空 |
| 网络不通 | 断网（或先 `ipconfig /release`）后点「登录并抓取课表」 | 提示「校务系统打不开，请检查校园网」，不弹异常栈 |

## 5. 回归（不该被 W6 弄坏）

- 「导入内置样本」仍能导入 3 门课；
- 「从剪贴板导入」空内容 → 「内容为空，请重新抓取」；
- 失败后再成功导入一次，列表正常刷新（不残留错误卡片）。

## 6. 记录

验收完成后把以下信息贴回任务（**不含账号密码、不含 Cookie**）：

```
Windows 版本 / WebView2 运行时版本：
首次登录：通过 / 不通过（现象）
会话复用：通过 / 不通过（现象）
失败路径：超时 / 改版 / 断网 = 通过 or 不通过
异常截图（如有）：
```
