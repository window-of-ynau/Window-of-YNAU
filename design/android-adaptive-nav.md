# Android 分层导航方案（v0.9.2 规格）

> 状态：**只出规格与线框，未改代码**。
> 依据：`design/layout-spec-v2.md`（桌面布局） + 现有实现 `app/lib/main.dart`（`_HomeShellState` / `_Sidebar` / `_SidebarItem` / `_IconColumnButton`）。
> 单位一律 **dp**；断点为**窗口宽度**（`MediaQuery.sizeOf(context).width`，不含安全区）。

---

## 0. 核心判断：两种范式，不是一套布局的缩放

| 平台 | 范式 | 入口可见性 | 为什么 |
|---|---|---|---|
| **Windows / 桌面** | **信息平层** | 侧栏常驻，所有入口一眼看到 | 鼠标精度高、屏幕宽、多任务切换频繁；侧栏占用 224 / 1280 = 17.5% 是划算的 |
| **Android / 手机** | **分层展示** | 侧栏**完全隐藏**，点汉堡才以覆盖层出现 | 390dp 宽的屏上，48dp rail = **12.3%** 屏宽永久被占；且 6 个图标无文字，靠记忆辨认（issue #84 说的"图标全同"） |

**这条差异必须写进代码结构**：`<600` 不再算"折叠的桌面侧栏"，而是**另一种布局**（没有 rail 这个概念）。

---

## 1. 三档断点（移动优先重定义）

| 档 | 宽度 | 导航形态 | 内容区 | 说明 |
|---|---|---|---|---|
| **手机** | `< 600` | **无 rail**；汉堡 → 覆盖层 Drawer（宽 280–320，含图标 + 文字 + 选中态） | 满宽，左右 padding **16** | 手机竖屏（360/390/412）与横屏都走这一档 |
| **折叠屏 / 平板** | `600 – 839` | **常驻 rail 64**（图标 + 选中态，长按/悬停显示文字） | 左右 padding **20** | 二选一的理由见下 |
| **桌面** | `≥ 840` | 常驻侧栏 224（展开）/ 64（折叠），保持不变 | 左右 padding **24** | 与 layout-spec-v2 一致 |

### 600–840 为什么选 rail 而不是 drawer

1. **宽度够**：600dp 上 64dp rail 只占 10.7%，且屏幕足够并排显示"图标 + 内容"，不需要模态遮挡；
2. **切换成本**：平板/折叠屏常被当作"小桌面"用（横屏 + 键盘），一次点击切页比"汉堡 → 抽屉 → 选 → 抽屉关闭"少两次交互；
3. **一致性**：640–840 的设备大概率能跑同一套窗口尺寸逻辑（折叠屏展开≈平板），和桌面共用 `collapsed` 状态最省心。

> 走 **rail 64** 而非 48：48 是 Material 的最小值，但按 10.9.5 的触控目标 ≥48×48 且图标 20 时，48 列左右各只剩 14 的内边距，视觉局促；64 列（左右各 22）与桌面的 `kIconColumn` 对齐更自然。

---

## 2. 覆盖层（Drawer）具体规格

| 项 | 数值 | 说明 |
|---|---|---|
| 抽屉宽度 | `min(320, screenWidth − 56)` | 390dp 手机 → **320**；360dp → **304**；320dp 小屏 → **264**（保证右侧至少露 56dp 的"可点回"区域） |
| 抽屉背景 | `tokens.surface` | A `#F7F7F7` / B 玻璃面 `rgba(255,255,255,.05)` / C `#FAF9F5` |
| 右侧圆角 | `0` | 抽屉是"整块侧壁"，不要圆角（圆角会像浮起的面板，与分层语义冲突） |
| 右侧分界 | **1px `tokens.hairline`** + 投影 `BoxShadow(color:#000 alpha .18, blur 16, offset (2,0))` | 与桌面同一条细线，保证跨平台观感一致 |
| 遮罩（scrim） | `Color(0x52000000)`（黑 32%） | 比 M3 默认 32% 相同；B 主题下再低一点也行（玻璃已有层次），但**不要低于 24%**，否则"后面还能点"的错觉 |
| 打开动画 | **246ms `Curves.easeOutCubic`** | M3 标准时长；位移 + 遮罩淡入同步 |
| 关闭动画 | **200ms `Curves.easeInCubic`** | 关闭比打开快，手感更利落 |
| 手势打开 | **左缘 20dp 内右滑**，需滑动 > 48dp 或速度 > 300dp/s 才判定为打开 | `Drawer` 组件自带 `drawerEdgeDragWidth: 20` |
| 手势关闭 | 抽屉上任意位置左滑；或点遮罩；或 **Android 系统返回键**（`PopScope`，Android 14 预测式返回） | 三选一都要通 |
| 内容（抽屉内） | 品牌区 48 + 分组标签 + 导航项（行高 **44**，图标 20，文字 14）+ 底部 `设置` | 行高 44 是触控目标底线（≥44×44），比桌面的 36 大 |
| 安全区 | 抽屉顶部 `paddingTop = statusBarHeight`；底部 `paddingBottom = gestureInset + 12` | 全面屏必须留 |

### 抽屉内部结构（与桌面 `_Sidebar` 同源，只改尺寸）

```
┌──────────────────────────────┐ ← paddingTop = statusBar
│  云农之窗            ✕       │ 44  品牌行（点 ✕ 或遮罩关闭）
│  对话                        │ 20  当前页名（inkSoft 12）
├──────────────────────────────┤
│  功能                        │ 12  分组标签（10/600/字距 .12em）
│  ▢  对话                     │ 44  ← 选中：surfaceAlt 底 + 左侧 3dp 强调条
│  校园 · 3                    │
│  ▦  我的课表                 │ 44
│  ▤  成绩查询                 │ 44
│  ▧  门户通知                 │ 44
│  ⚙  设置                     │ 44  ← 贴底
└──────────────────────────────┘
```

---

## 3. 手机两态线框图（390 × 844，含安全区）

### 3.1 关闭（默认）——**没有 rail，内容全宽**

```
┌──────────────────────────────────────┐ ← statusBar  (24)
│ ☰   云农之窗                    ⚙    │ 44  状态行（现 kStatusRowHeight=44）
├──────────────────────────────────────┤
│                                      │
│                                      │
│         今天想从哪儿开始？            │
│      问点什么吧——课表、成绩、…        │
│                                      │
│   [第 8 周哪天最累？] [导出日历]      │
│   [考核方式] [几门课？]                │
│                                      │
│                                      │
├──────────────────────────────────────┤
│  ┌──────────────────────────┐  [发送] │  composer 56 + 底部 inset 12
└──────────────────────────────────────┘ ← gestureInset (16)
   ☰ 按钮：48×48 触控区，左 padding 16 → 中心 x=40
   内容左右 padding 16
```

### 3.2 打开（覆盖层）——抽屉 320 + 遮罩 32%

```
┌──────────────────────────────────────┐ ← statusBar
│┌───────────────────────────┐┌───────┐│
││ 云农之窗            ✕      ││       ││ 抽屉 320
││ 对话                       ││ 内容  ││
│├───────────────────────────┤│ 被遮罩 ││
││ 功能                       ││ 盖住  ││
││ ▐▢  对话                   ││（32%  ││
││ 校园 · 3                   ││ 黑）  ││
││  ▦  我的课表               ││       ││
││  ▤  成绩查询               ││       ││
││  ▧  门户通知               ││       ││
││                            ││       ││
││                            ││       ││
││ ⚙  设置                    ││       ││
│└───────────────────────────┘└───────┘│
│   composer 被抽屉 + 遮罩整体覆盖      │ ← 见 §5 的取舍
└──────────────────────────────────────┘
```

---

## 4. 与现有实现的差异清单（按代码里的名字）

| 组件 / 文件 | 现状 | v0.9.2 目标 |
|---|---|---|
| `_HomeShellState.kRailWidth = 48` | 窄屏常驻 48 rail | **手机档不再使用**；平板档改为 **64**（`kRailWidthTablet = 64`） |
| `_HomeShellState.build` 里的 `final bool narrow` | `narrow = width < 600` → 走 rail | 拆成三档：`phone / tablet / desktop`；`phone` 分支**完全不渲染 `rail`**，改为 `Scaffold(drawer: …, endDrawer: null)` |
| `Scaffold`（现在没有 drawer） | 只有 `body: Row(...)` | 手机档加 `drawer: _CoverDrawer(...)`、`drawerEdgeDragWidth: 20`、`drawerScrimColor: Color(0x52000000)`、`drawerEnableOpenDragGesture: true` |
| `rail`（`Column[汉堡, _Sidebar, …]`） | 常驻 | 仅 tablet / desktop 渲染；phone 不渲染 |
| `_IconColumnButton`（汉堡键 `Key('sidebar-toggle')`） | 在 rail 顶部 | phone 档改放进**状态行左侧**（`_ChatPane` / `_CampusPane` 的头部行），点击调 `Scaffold.of(context).openDrawer()` |
| `_Sidebar` | 支持 `collapsed` / `narrow` 两种密度 | 新增 `inDrawer: true` 模式：行高 36 → **44**、宽度由抽屉决定、始终带文字（不再有 collapsed 态）、顶部加品牌行 + 关闭按钮、底部 `设置` 贴底 |
| `_SidebarItem` | collapsed 时是 `IconButton` | 抽屉内**不用** collapsed 分支，统一走"图标 20 + 文字 14"的行 |
| `_ChatPane` / `_CampusPane` | 状态行 44 内已有汉堡位（`Key('sidebar-toggle')`） | 手机档：汉堡 48×48、左 padding **16**（不再按 64 图标列对齐，因为手机没有 rail 需要对齐） |
| `_ChatPane` 的 composer | 底部固定 | 手机档：底部 `paddingBottom = gestureInset + 12`；抽屉打开时由 `Drawer` 自然覆盖（见 §5） |
| `_HomeShellState._toggleSidebar` | 改 `_collapsedOverride` | 手机档**不再调用**它（改为 open/close drawer）；平板/桌面保持原逻辑 |
| `_sideBarCollapsedKey`（store 里的 `sidebar.collapsed`） | 跨设备共享 | 建议**按平台分开存**：`sidebar.collapsed.desktop` / `.tablet`；手机档不读不写 |
| `android/app/src/main/AndroidManifest.xml`（如有 `screenOrientation`） | 未限制 | 保持不限制（横屏也走 phone 档，见 §1） |

> 建议新增一个纯函数集中判档，避免散落判断：
> ```dart
> enum NavMode { phoneDrawer, tabletRail, desktopSidebar }
> NavMode navModeOf(double width) =>
>     width < 600 ? NavMode.phoneDrawer
>   : width < 840 ? NavMode.tabletRail
>   : NavMode.desktopSidebar;
> ```

---

## 5. composer 与抽屉的关系（你必须回答的那个细节）

**结论：手机档采用「全高抽屉」，composer 被覆盖；抽屉打开时自动收起键盘。**

理由（三条，按重要性）：

1. **导航是模态操作**：抽屉里选完就会离开当前页，此时 composer 即使可见也**不可用**；"可见但点不动"比"看不见"更让人困惑，还会诱发误触；
2. **键盘会顶起来**：手机上打开抽屉前往往键盘已弹起（用户刚在输入框里），如果抽屉只到 composer 顶部，键盘一收一放会让抽屉高度跳变；
3. **手势一致**：全高抽屉可以整面左滑关闭；"半高抽屉"会让顶部 44 的状态行仍可点，返回层级变得含糊。

**如果你更希望 composer 一直可见**，改成「上推式半高抽屉」也可行，规格如下（实现成本 +0.5 天）：

| 项 | 全高抽屉（推荐） | 半高抽屉（可选 B） |
|---|---|---|
| 抽屉高度 | `screenHeight` | `screenHeight − composerBlock`，`composerBlock = 56 + gestureInset + 12 + 16`（≈100） |
| 遮罩范围 | 整屏 | 只盖内容区（不盖 composer） |
| composer 交互 | 不可用（被覆盖） | **仍可用**（点输入框 → 自动关抽屉） |
| 键盘 | 打开抽屉前收起 | 用户点输入框时再弹 |
| 风险 | 遮盖 composer（正是 owner 担心的） | 抽屉看起来像"弹层菜单"，与桌面侧栏的"壁"感不一致；返回手势歧义 |

> 我的建议是**先做全高抽屉**（符合 M3、实现最快、最不容易出 bug），v0.9.2 上手后再看是否要 B。

---

## 6. 风险与取舍

| 风险 | 说明 | 缓解 |
|---|---|---|
| **遮住内容**（与"平层"相反） | 抽屉打开时看不到正在看的课表/对话 | 只在导航时打开、选完立刻关闭；抽屉里给出**当前页高亮**，减少"我该点哪个"的犹豫 |
| **入口不可见** | 手机首屏看不到"课表/成绩"入口 | 空态与状态行保留**一个跳转入口**（例如空态 chip 里已有"第 8 周哪天最累？"），并在**首次安装**时自动展开一次抽屉做引导（一次性，记 `nav.onboarded`） |
| **和桌面测量不一致** | 手机 16 / 平板 20 / 桌面 24 三套 padding，容易写死 | 统一用 `contentPadding(width)` 函数（layout-spec-v2 §1 已有） |
| **横屏手机** | 横屏 844×390 仍走 phone 档，抽屉高 390 但内容很短 | 横屏时抽屉 `maxHeight` 不额外限制；导航项 6 条 × 44 = 264 < 390，够用 |
| **无障碍** | 抽屉要能被 TalkBack 正确朗读 | `Drawer` + `Semantics(label: '导航菜单')`；汉堡键语义标签「打开导航菜单」 |

---

## 7. 验收清单（实现后可直接照这条测）

1. **无 rail**：宽度 390 时，`find.byKey(const Key('sidebar-toggle'))` 在状态行内（`y < 60`），且页面左侧**没有** 48dp 常驻条（可用 `tester.getTopLeft(find.byType(_Sidebar))` 断言不存在）；
2. **抽屉宽度**：390 → 320；360 → 304；320 → 264（`min(320, w − 56)`）；
3. **打开方式**：点汉堡打开、左缘 20dp 右滑打开、系统返回键关闭；
4. **遮罩**：颜色 `0x52000000`，点遮罩关闭；
5. **选中态**：抽屉里当前页有 `surfaceAlt` 底 + 左侧 3dp 强调条；
6. **640 平板**：仍是 rail 64（不是抽屉）；**1024 桌面**：侧栏 224；
7. **安全区**：iPhone 式/安卓手势条下 composer 不被遮挡（`paddingBottom = inset + 12`）。

---

## 8. 配套视觉稿（可复现）

| 文件 | 内容 |
|---|---|
| `design/android-nav-mockups/phone-closed-a.png` | 手机 390×844 **关闭态**（A 主题）：无 rail、内容全宽、汉堡在状态行左侧 48×48 |
| `design/android-nav-mockups/phone-open-a.png` | **打开态**（A 主题）：抽屉 320 + 遮罩 32%，选中项 `surfaceAlt` 底 + 左侧 3dp 条 |
| `design/android-nav-mockups/phone-open-c.png` | **打开态**（C 主题）：抽屉纸面 `#FAF9F5`、强调条 `#A9583E` |
| `design/android-nav-mockups/preview.png` | 三张并排总览 |
| `design/android-nav-mockups/phone.html` | 源文件（`?open=0` 或 `?open=1`，`t=a` 或 `t=c`），尺寸即规格数值 |

视觉稿里**抽屉是 320、遮罩 32%、行高 44、选中条 3dp**——这四个数值请直接照抄，不要在实现时"看着差不多"再调。
