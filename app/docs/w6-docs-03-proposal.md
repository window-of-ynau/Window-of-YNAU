# W6 文档补丁提案（待 captain/单写者合入 docs/03、docs/04）

> 任务要求把 W6 的登录流程/会话存放/登录态规则/失败码写进 `docs/03 §7（端侧本地接口）`，
> 但本任务的硬约束是「只在 `app/**` 与 `crates/core/**` 内改」，`docs/**` 属 captain/单写者。
> 所以这里给出**可直接粘贴**的补丁文本，我不动 `docs/`。
>
> 说明：WebView 桥现在是 `docs/03 §8`，`§7` 是 Rust 端侧接口；建议补在 **§8**（桥），
> §7.5 只补一句交叉引用。

---

## 1. `docs/03 §8` 增补（建议在 8.2 之后新增 8.3）

### 8.3 登录流程与会话（W6）

**流程**（`app/lib/src/campus/webview_fetcher.dart`）

```text
点「登录并抓取课表」
  → 打开课表页 CampusEntry.timetable.url
       ├─ 会话有效：直接就是课表页 → 探针看到 table#timetable → 自动抓取
       └─ 会话无效：CAS 带到统一身份认证登录页 → 用户登录（含验证码/二次验证）
                      → 跳回课表页 → 探针看到业务元素 → 自动抓取（无需再点按钮）
  → 抓取 table#timetable 的 outerHTML → bridge 回传 → 关页面
  → 端侧 parse（PARSE_FAILED 直接失败）→ SQLite（terms/courses）→ 列表
```

**会话存放位置（Cookie 只在本机）**

| 平台 | 位置 | 说明 |
|---|---|---|
| Windows | `%LOCALAPPDATA%\yunong\webview\`（WebView2 user data folder） | 必须在创建 WebView 之前用 `WebViewEnvironmentSettings(userDataFolder:)` 指定；放临时目录等于每次重启都要重新登录 |
| Android | 应用私有目录（系统管理，`.../app_webview/`） | 系统 CookieManager 持久化；需 `thirdPartyCookiesEnabled`（CAS 与业务系统不同域） |

Cookie **不进** SQLite、**不进** JSON、**不进**日志、**不上传**；端侧从不读 `document.cookie`。

**登录态判定规则**（`kCampusProbeScript`，只观察 DOM/可见文本）

| 优先级 | 信号 | 判定 | reason |
|---|---|---|---|
| 1 | `table#timetable` / `table.timetable` / `#timetable` | 已登录 | `timetable` |
| 2 | `#xh_id` / `.user-info` / `.stu-name` / `#topMenu`，或可见文本 `学号：\d{6,}`、`姓名：…` | 已登录 | `student_bar` |
| 3 | iframe 内出现课表表格（同域可读时） | 已登录 | `timetable_iframe` |
| 4 | `input[type=password]` / `#casLoginForm` / `form[action*="login"]` | 需登录 | `login_form` |
| 5 | 可见文本 `请登录/未登录/登录超时/重新登录` | 需登录 | `login_hint` |
| — | 都没有 | 未知（提示"页面可能改版"） | `no_signal` |

**为什么不用 URL 判**：CAS 在 `jwxt`/`qzjwxt`/统一身份认证之间来回跳，URL 变化既不代表登录成功
也不代表失败；业务元素才是事实。上报的 `path` **去掉 query/fragment**（CAS 的 `ticket` 是凭据）。

**抓取失败码**（bridge → 端侧，用户可见文案由 `describeFetchFailure` 统一）

| code | 触发 | 提示 |
|---|---|---|
| `HTML_CHANGED` | 找不到 `table#timetable`（自动重试 ≈30s 后仍没有） | 学校页面可能已更新，请反馈 |
| `LOGIN_TIMEOUT` | 5 分钟内未登录/未抓到 | 登录超时，请重试 |
| `NETWORK` | 域名解析失败 / 超时 / 连接失败 | 校务系统打不开，请检查校园网 |
| `CERT` | 证书错误 | 校务系统证书异常，请检查网络环境 |
| `NO_WEBVIEW` | 平台无内嵌浏览器 | 当前平台不支持内置浏览器 |
| `CANCELLED` | 用户点"取消" | 已取消抓取 |

**不变量**：解析失败或抓取失败**绝不**写入空课表、**绝不**清空已有列表（沿用 §8.2 的失败姿势）。

---

## 2. `docs/03 §7.5` 增补一句

> 课表 HTML 由 WebView 抓取（见 §8.3 登录流程与会话），端侧 `parse_timetable` 只接受 HTML 文本，
> 不接触 Cookie；`CourseStore::import_report` 对空解析结果报错且不清库。

---

## 3. ADR 提案（`docs/04-ADR.md`，编号请按现有序列分配）

```markdown
### ADR-17 校务会话存放在 WebView 自身 profile，不进端侧数据库

- **状态**：已接受（W6）
- **背景**：W6 要求"重启免二次登录"，同时 ADR-02/ADR-11 要求凭据不出端、不入库、不进日志。
  可选方案：① 自己抓 Set-Cookie 存 keystore 再注入；② 交给平台 WebView 的会话存储。
- **决策**：采用 ②。Windows 用 WebView2 user data folder（`%LOCALAPPDATA%\yunong\webview\`），
  Android 用系统 CookieManager（应用私有目录）。端侧代码**从不读取** Cookie 值。
- **理由**：① 需要解析/重放 `Set-Cookie`（Domain/Path/SameSite/HttpOnly 都得自己算），
  还要处理 CAS 的多域跳转，出错面大且容易把凭据落进我们自己的存储；② 平台实现天然满足
  HttpOnly 与同源策略，存储介质由 OS 账户/应用沙箱隔离，**凭据根本不经过我们的代码**。
- **代价**：卸载/清理应用数据会丢会话（可接受：重新登录一次）；
  跨平台行为差异（Windows 目录可见、Android 沙箱）需要在文档中写清。
- **不变量**：Cookie 不进 SQLite / JSON / 日志 / 上传；端侧只读页面可见的业务元素判定登录态。
- **验收**：`app/docs/w6-manual-acceptance.md` 的"会话复用"与"存放位置核对"两节。
```
