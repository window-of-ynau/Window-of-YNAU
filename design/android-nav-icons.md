# Android 导航图标规范（#84 定稿）

> 状态：**定稿**，只出规格与视觉稿，**未改代码**。
> 配套：`design/android-adaptive-nav.md`（#90 分层导航，本文补它的 #84 图标部分）· `design/android-nav-mockups/`（视觉稿与可复现源文件）。
> 依据：`flutter/packages/flutter/lib/src/material/icons.dart`（**Flutter 3.47.4，codepoint 已逐条核对**）、`app/lib/src/theme/yunong_tokens.dart` + `themes.dart`（三主题 token）、`app/lib/src/campus/campus_entries.dart`、`app/lib/main.dart`（`_Sidebar` / `_SidebarItem` / `_IconColumnButton`）。
> 单位一律 **dp**；图标渲染尺寸 **20 × 20**；颜色**一律走 token，不写死 hex**。

---

## 0. 一句话结论

#84 的根因不是「图标库不够大」，是 **6 个入口共用同一枚 `Icons.school_outlined`** —— `main.dart:607` 的 `for` 循环把 `icon:` 写死了，所以「图标全同、只能靠记忆认」。
修法：**每个入口一枚语义化、轮廓互不相同的 Material Outlined 图标**，20dp、单色、颜色只取 `inkSoft / ink / accent` 三个 token。

---

## 1. 现状 → 目标（#84）

| 项 | 现状（代码事实） | 目标 |
|---|---|---|
| 入口图标 | `_Sidebar`（main.dart:604-611）对 `entries` 全部用 `Icons.school_outlined`；对话 `chat_bubble_outline`、设置 `settings_outlined` 各自不同 | 每个入口一枚**独立**语义图标（见 §2），任意两枚轮廓不相似 |
| 选中态图标色 | `_SidebarItem`（main.dart:672）恒 `tokens.inkSoft`，选中不变色（只有文字变 `ink`/600） | 选中图标 `tokens.ink`（见 §4/§5） |
| 图标尺寸 | 混用：rail 里 `iconSize: 20`、图标列却按 48 布局（与 layout-spec-v2 的 64 不一致） | 全部 **20dp**，图标列统一 64（由 t6 落地，见 android-adaptive-nav.md §9） |
| 可辨识性 | 6 个图形 5 个一样 → 无法凭形状区分（#84 原话「图标全同」） | 抽屉带文字 + rail 带 tooltip + 形状各异，三重兜底 |

---

## 2. 9 个语义槽位（6 个主入口 + 3 个现有校务入口）

> 6 个主入口按 #84 指定：**课表 / 成绩 / 规章 / 校园生活 / 对话 / 设置**。
> 现有 `CampusEntry`（`portal / scores / timetable / library / software`）**全部**给到独立图标，避免「换完 6 个、剩 3 个还是同一个」的复发。

| 槽位 ID | 入口文案 | 首选 Flutter `IconData` | codepoint | 备选 | 语义理由（为什么选它） |
|---|---|---|---|---|---|
| `chat` | 对话 | `Icons.chat_bubble_outline` | `e155` | `Icons.forum_outlined` | 圆角对话气泡 + 左下小尾，唯一「说话」轮廓；沿用现状 |
| `timetable` | 我的课表 / 课表 | `Icons.calendar_month_outlined` | `f051f` | `Icons.calendar_today_outlined`（`ef11`） | 日历网格 + 表头，读作「周课表」；**不再用毕业帽 `school`** |
| `scores` | 成绩查询 / 成绩 | `Icons.assessment_outlined` | `ee95` | `Icons.insert_chart_outlined` | 文档轮廓 + 三根升序柱，读作「成绩单 / 统计」 |
| `regulations` | 规章（校规 / 制度，RAG 文档） | `Icons.menu_book_outlined` | `f1c2` | `Icons.rule_outlined`（`f32b`） | 摊开书本，读作「规章制度汇编」 |
| `campus` | 校园生活（社团 / 生活服务） | `Icons.groups_outlined` | `f0db` | `Icons.diversity_3_outlined` | 三人组，读作「社团 / 校园生活」；和「建筑」类（门户/图书馆）区分开 |
| `settings` | 设置 | `Icons.settings_outlined` | `f36e` | — | 齿轮，通用设置语义；沿用现状 |
| `portal` | 教务门户 | `Icons.account_balance_outlined` | `ee32` | `Icons.location_city_outlined` | 三角顶 + 立柱 + 台基，读作「教务大厅 / 机构」 |
| `library` | 图书馆 | `Icons.local_library_outlined` | `f181` | `Icons.auto_stories_outlined` | 人头 + 摊开书，读作「图书馆」；与「规章」的书本靠人头区分 |
| `software` | 正版软件 | `Icons.verified_outlined` | `f47c` | `Icons.check_circle_outline` | 印章轮廓 + 对勾，读作「正版 / 已验证」 |

**界面操作图标（非入口，同一 20dp 规范）**：`menu`（`e3dc`，汉堡）、`menu_open`（`e3de`，展开态）、`close`（`e16a`，抽屉关闭）。

> 全部 codepoint 已在本机 Flutter 3.47.4 的 `icons.dart` 核对存在，且与 `materialicons-regular.otf` 一致 —— 视觉稿用**同一套字体**渲染，**视觉稿 == 实现**，不需要另造 SVG 资产。

---

## 3. 20dp 描边图标规范

| 项 | 规格 | 说明 |
|---|---|---|
| 网格 | Material Icons **24dp 母版网格** | 所有 `Icons.*` 天然满足 |
| 渲染尺寸 | **20 × 20 dp**（`Icon(size: 20)` / 字号 20px） | 抽屉行、rail 按钮、桌面侧栏三处统一 |
| 线宽 | Outlined 固有 ≈ 2dp @24 网格 → **20dp 时 ≈ 1.67dp** | 若自绘（P2）：stroke **1.6–1.7dp**，`stroke-linecap/join: round`，最小不低于 **1.25dp** |
| 填充 | **FILL 0，纯描边**；单色 | 唯一例外：P2「今日格」可用 `accent` 实心 6×4dp（见 §7.7） |
| 光学边距 | 有效图形落在 20×20 内并留 **≥1dp** 视觉边距；同类图标重心一致、大小均衡 | 避免一大一小（例如 `verified` 天生偏满，配 20dp 时不再放大） |
| 笔画密度 | 每图标 ≤ 4–5 段 | 20dp 下超过就糊；Material Outlined 已达标 |
| 对齐 | 在 20dp 盒内居中；抽屉里图标槽 = 图标 20 + 间距 8（文字起点 x = 16+20+8 = **44**） | rail 里按钮 40×40 居中于 64 列 → 图标中心 x = **32** |

---

## 4. 三主题配色（token 绑定，不新增色值）

**规则（只有三条）**：
1. **默认（未选中）图标 = `tokens.inkSoft`**
2. **选中图标 = `tokens.ink`**
3. **强调元素（今日格 / 选中条）= `tokens.accent`**

其余一律禁止：不许在 `Icon` 上写 `Color(0x…)`，不许给图标单独定义主题色 —— 与 `yunong_tokens.dart` 顶部「widget 里不许出现字面量颜色」同一条纪律。

| 主题 / 亮度 | 默认图标 `inkSoft` | 选中图标 `ink` | 强调元素 `accent` | 选中行底 `surfaceAlt` | 默认图标对比度（on surface，非文本 ≥3:1） |
|---|---|---|---|---|---|
| A 极简黑白 · 浅 | `#6B6B6B` | `#0D0D0D` | `#0D0D0D` | `#F0F0F0` | **4.97 : 1** ✅ |
| A 极简黑白 · 深 | `#A8A8A8` | `#F5F5F5` | `#F5F5F5` | `#1C1C1C` | **7.8 : 1** ✅ |
| B 深蓝玻璃 · 浅 | `#3C5170` | `#0B1A33` | `#2E56CE` | `#E6EEFF` | **7.8 : 1** ✅ |
| B 深蓝玻璃 · 深 | `#A9BFF0` | `#E8EEFC` | `#3F68E4` | `rgba(255,255,255,.10)` | **9.9 : 1** ✅ |
| C 书本质感 · 浅 | `#6C6A64` | `#141413` | `#A9583E` | `#EFEEE4` | **5.1 : 1** ✅ |
| C 书本质感 · 深 | `#B9A88E` | `#F0E6D8` | `#DA7756` | `#2E271E` | **7.1 : 1** ✅ |

> 对比度为按 token 实算的估算值（WCAG 相对亮度公式），全部远高于图形元素 3:1 门槛；C 的 `accent` 只做 3dp 条/图标（`#A9583E` on 纸 4.8:1），符合「橙色只做强调、不大面积填充」的既有约束（见 `theme-research.md` §3）。

---

## 5. 状态与容器规则（三处复用同一套图标）

| 位置 | 行高 / 按钮 | 图标 | 默认 | 选中 |
|---|---|---|---|---|
| **抽屉（手机 <600）** | 行 **44**；左 padding **16**；图标槽 20 + 间距 8；文字 14 | 20 | `inkSoft` | `surfaceAlt` 底 + **左 3dp `accent` 条**（上下各内缩 10，圆角 2）+ 图标 `ink` + 文字 14 / **600** |
| **rail（平板 600–839；桌面折叠）** | 列 **64**；按钮 **40×40**（触控目标 ≥48） | 20 | `inkSoft` + 长按/悬停 tooltip 显示文字 | 图标 `ink` + **rail 左缘 3×24 `accent` 条** |
| **侧栏展开（≥840）** | 行 **36**；图标列 64（图标中心 **x=32**） | 20 | `inkSoft` | 与抽屉同语言：`surfaceAlt` 底 + 3dp 条 + `ink` + 600 |
| 品牌行 / 汉堡 / 关闭 | 44 / 48×48 | 20 | `inkSoft`（hover → `ink`） | — |

- 动效：颜色切换跟 `tokens.fast`（120–140ms）淡变；**不做缩放/旋转**（导航图标要稳）。
- 选中只改**颜色 + 容器**，**不改形状**（同一枚 glyph 三处复用，避免三套资产）。

---

## 6. 无障碍

- 导航行 `Semantics(label: '<入口名>', selected: <bool>)`；抽屉整体 `Semantics(label: '导航菜单')`。
- 汉堡按钮语义标签「打开导航菜单」，关闭按钮「关闭导航菜单」。
- 触控目标不缩水：抽屉行 44（≥44×44 底线）；rail 按钮 48（≥48×48）；桌面 36 行外扩点击区到 ≥40。
- 对比度见 §4（全部 ≥3:1，多数 ≥4.5:1）。

---

## 7. 实现指引（**只是落点清单，业务代码由 lead-client / t6 写**）

1. `app/lib/src/campus/campus_entries.dart`：给 `CampusEntry` 加 `IconData get icon`（或同文件静态函数），映射
   `portal→account_balance_outlined`、`scores→assessment_outlined`、`timetable→calendar_month_outlined`、`library→local_library_outlined`、`software→verified_outlined`。
2. `main.dart` `_Sidebar`（≈596–611）：把 `icon: Icons.school_outlined` 改成 `icon: entries[i].icon`。
3. `_SidebarItem`（≈672）：`Icon(icon, color: selected ? tokens.ink : tokens.inkSoft)`。
4. 新抽屉 `_CoverDrawer`：对话 = `Icons.chat_bubble_outline`，设置 = `Icons.settings_outlined`，其余复用 §2 映射；行结构照 §5（行高 44 / 左 padding 16 / 图标槽 20+8 / 3dp 条）。
5. 汉堡 / 关闭：`Icons.menu` / `Icons.menu_open` / `Icons.close`。
6. 常量：`kRailWidth`、`kIconColumn` 由 48 → **64**（平板档口径见 `android-adaptive-nav.md` §1 与 `layout-spec-v2.md` §2）。
7. **P2 可选增强「今日格」**：在 `calendar_month_outlined` 的左下格叠加 **6×4dp、圆角 1dp、`tokens.accent` 实心块**（`CustomPainter` 叠画，不改字形）；本轮可不上，验收不依赖它。

---

## 8. 验收清单（实现后可直接照这条测）

1. **图标唯一（#84 回归）**：抽屉 / rail / 侧栏里所有入口的 `IconData` **两两不同**（`tester.widgetList<Icon>(...).map((i) => i.icon).toSet().length == 入口数`）。
2. **尺寸**：每个入口图标 `tester.getSize(find.byIcon(...)) == Size(20, 20)`。
3. **颜色**：默认 == `tokens.inkSoft`，选中 == `tokens.ink`（A / B / C 三主题各取一次；含明暗两态）。
4. **选中态容器**：抽屉行有 `surfaceAlt` 底 + 左 **3dp** `accent` 条；rail 选中项有 **3×24** `accent` 条且图标为 `ink`。
5. **不硬编码颜色**：`grep -R "Icon(.*color: *Color(" app/lib` 无命中（图标颜色全部来自 `tokens.*`）。
6. **对比度**：默认图标 on surface ≥ 3:1（§4 表已给六组估算值，可直接断言）。
7. **TalkBack**：每行读出「<入口名>，已选中 / 未选中」；汉堡读「打开导航菜单」。
8. **视觉稿一致性**：`icons-sheet.png` 的 9 枚字形与 App 实际渲染一致（同一 MaterialIcons 字体，无需对图微调）。

---

## 9. 视觉稿与源文件清单（可复现）

| 文件 | 内容 |
|---|---|
| `design/android-nav-mockups/icons-sheet.png` | **三主题 × 9 槽位对照**：默认 20dp / 选中态行片段（surfaceAlt + 3dp 条 + ink）/ 48dp 放大 + 底部 token 配色表 |
| `design/android-nav-mockups/phone-open-a.png` `phone-open-b.png` `phone-open-c.png` | 手机抽屉三主题（A 黑白 / B 深蓝玻璃 / C 书本质感），真实图标 + 选中态 |
| `design/android-nav-mockups/phone-closed-a.png` | 手机无 rail 关闭态（仅汉堡 + 设置，无入口图标） |
| `design/android-nav-mockups/tablet-rail-a.png` | 平板 700×800：常驻 rail 64 + 选中 3×24 条 + 长按/悬停 tooltip |
| `design/android-nav-mockups/preview.png` | 四张手机态并排总览 |
| `design/android-nav-mockups/icons.html` / `phone.html` / `tablet.html` | 源文件（`?open=0|1&t=a|b|c`），尺寸即规格数值 |
| `design/android-nav-mockups/nav-icons.js` | 9 槽位 codepoint + 中文名 + 语义理由（**图标唯一来源**） |
| `design/android-nav-mockups/assets/materialicons-regular.otf` | 与 Flutter 同源的 MaterialIcons 字体（Apache-2.0，许可证同目录） |

> 视觉稿里**抽屉 320 / 遮罩 32% / 行高 44 / 选中条 3dp / 图标 20dp** 这五个数值与 `android-adaptive-nav.md` §8 完全一致，请直接照抄。
