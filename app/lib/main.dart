import 'dart:async';

// 云农之窗 · 客户端骨架（W1）
//
// 形态已定（ADR-07 / T-02）：**Flutter 原生 UI**，只在打开校务系统时内嵌 WebView。
// 所以这里看到的是原生导航 + 原生对话区，而不是"整个 App 是一层网页壳"。
//
// 本文件只负责结构与导航；真正的对话、课表、日程在后续里程碑接入 Rust 核心（FFI）。

import 'package:flutter/material.dart';

import 'src/campus/campus_entries.dart';
import 'src/campus/timetable_page.dart';
import 'src/core/app_paths.dart';
import 'src/core/titlebar_service.dart';
import 'src/chat/chat_settings_section.dart';
import 'src/core/auth_service.dart';
import 'src/core/secret_service.dart';
import 'src/theme/file_settings_store.dart';
import 'src/chat/chat_page.dart';
import 'src/campus/webview_fetcher.dart';
import 'src/core/timetable_service.dart';
import 'src/settings/settings_page.dart';
import 'src/theme/theme_controller.dart';
import 'src/theme/glass_panel.dart';
import 'src/theme/paper_texture.dart';
import 'src/theme/glass_performance.dart';
import 'src/theme/themes.dart';
import 'src/theme/yunong_tokens.dart';

void main() {
  WidgetsFlutterBinding.ensureInitialized();
  runApp(const YunongApp());
}

/// 应用根：**唯一**持有 [ThemeController] 的地方。
///
/// 换主题 = `controller.notifyListeners()` → `MaterialApp` 重建 →
/// `AnimatedTheme` 做 200ms 过渡。不需要重启，也不需要重建页面状态
/// （`home` 始终是同一个 `HomeShell`，路由栈不变）。
class YunongApp extends StatefulWidget {
  const YunongApp({super.key, this.themeController, this.titleBarBackend});

  /// 测试可注入；生产为 null（用真 FFI，失败静默）
  final TitleBarBackend? titleBarBackend;

  /// 测试可注入；生产为 null（现造一个，**文件持久化**存储）。
  final ThemeController? themeController;


  @override
  State<YunongApp> createState() => _YunongAppState();
}

class _YunongAppState extends State<YunongApp> {
  late final ThemeController _controller =
      widget.themeController ?? ThemeController(store: FileSettingsStore());

  late final TitleBarBackend _titleBar = widget.titleBarBackend ?? FfiTitleBarBackend();

  /// 玻璃帧率闸门：**只在 B（深蓝玻璃）主题下挂载**，A/C 完全不启动。
  final GlassGovernor _glass = GlassGovernor();

  @override
  void initState() {
    super.initState();
    // 读回上次选择（默认 A 极简黑白）。存储未接时是内存实现，读不到就用默认值。
    unawaited(_controller.load());
    _controller.addListener(_syncGlassGovernor);
    _syncGlassGovernor();
    // 标题栏跟随主题：启动一次 + 每次切换（见 _syncTitleBar）
    unawaited(_syncTitleBar());
  }

  Future<void> _syncTitleBar() => syncTitleBar(_titleBar, _controller.themeId);

  void _syncGlassGovernor() {
    unawaited(_syncTitleBar());
    final bool needsGlass = _controller.themeId == AppThemeId.deepGlass &&
        tokensFor(AppThemeId.deepGlass, Brightness.dark).glass != null;
    if (needsGlass) {
      _glass.attach();
    } else {
      _glass.detach();
    }
  }

  @override
  void dispose() {
    _controller.removeListener(_syncGlassGovernor);
    _glass.dispose();
    if (widget.themeController == null) {
      _controller.dispose();
    }
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return GlassScope(
      governor: _glass,
      child: ListenableBuilder(
        listenable: _controller,
        builder: (BuildContext context, Widget? child) {
          return MaterialApp(
            title: '云农之窗',
            debugShowCheckedModeBanner: false,
            // WebView 抓取页需要根导航（CampusWebViewPage 全屏 push）
            navigatorKey: WebViewSchoolFetcher.navigatorKey,
            theme: _controller.theme(Brightness.light),
            darkTheme: _controller.theme(Brightness.dark),
            // P1-3：不再单独选明暗；主题自身决定（B 深蓝玻璃=深色，A/C=浅色）
      themeMode: _controller.themeId == AppThemeId.deepGlass
          ? ThemeMode.dark
          : ThemeMode.light,
            // 画布背景：B 的深蓝渐变、C 的纸纹都铺在这里（Scaffold 对这些主题透明），
            // 这样所有路由（设置页、课表页）都叠在同一层背景上，切页不会"闪白"。
            builder: (BuildContext context, Widget? child) {
              final YunongTokens tokens = context.tokens;
              Widget content = child ?? const SizedBox.shrink();
              if (tokens.canvasGradient != null) {
                content = DecoratedBox(
                  decoration: BoxDecoration(gradient: tokens.canvasGradient),
                  child: content,
                );
              }
              final TextureSpec? texture = tokens.texture;
              if (texture != null) {
                // 纸纹：单次 drawRect 平铺（见 paper_texture.dart 顶部的事故记录）。
                // 旧写法 Image.asset(repeat: …) 在 2560×1440 下是 57,600 次绘制/帧。
                content = PaperTexture(spec: texture, child: content);
              }
              return content;
            },
            home: HomeShell(themeController: _controller),
          );
        },
      ),
    );
  }
}

/// 左侧导航 + 右侧主区（PRD §4 信息架构）。
class HomeShell extends StatefulWidget {
  const HomeShell({super.key, required this.themeController});

  final ThemeController themeController;

  @override
  State<HomeShell> createState() => _HomeShellState();
}

class _HomeShellState extends State<HomeShell> {
  @override
  void initState() {
    super.initState();
    _initSharedServices();
  }

  int _selected = 0;

  @override
  Widget build(BuildContext context) {
    const entries = CampusEntry.values;
    final destination = _selected == 0 ? null : entries[_selected - 1];

    return Scaffold(
      // 玻璃主题：AppBar 底下垫一层玻璃（全屏只有这一层会真模糊）；
      // A/C 的 tokens.glass == null，GlassPanel 退化成普通表面 → 0 个 BackdropFilter。
      appBar: AppBar(
        backgroundColor: context.tokens.glass == null
            ? context.tokens.canvas
            : Colors.transparent,
        flexibleSpace: context.tokens.glass == null
            ? null
            : const GlassPanel(
                role: GlassRole.appBar,
                borderRadius: BorderRadius.zero,
                child: SizedBox.expand(),
              ),
        title: Text(destination?.label ?? '云农之窗'),
        actions: <Widget>[
          IconButton(
            tooltip: '设置',
            onPressed: _openSettings,
            icon: const Icon(Icons.settings_outlined),
          ),
        ],
      ),
      drawer: Drawer(
        // 分组侧栏（t76 视觉新增）：上组「功能」、下组「校园 · N」。
        // 只改分组标签与密度；「设置」入口仍统一走 AppBar 的齿轮（不回退 main 的统一入口）。
        child: ListView(
          padding: const EdgeInsets.only(top: 8, bottom: 12),
          children: <Widget>[
            Padding(
              padding: const EdgeInsets.fromLTRB(18, 20, 18, 8),
              child: Text('云农之窗', style: Theme.of(context).textTheme.headlineSmall),
            ),
            const _DrawerSectionLabel(text: '功能'),
            ListTile(
              dense: true,
              leading: const Icon(Icons.chat_bubble_outline),
              title: const Text('对话'),
              selected: _selected == 0,
              onTap: () => _select(0),
            ),
            _DrawerSectionLabel(text: '校园 · ${entries.length}'),
            for (var i = 0; i < entries.length; i++)
              ListTile(
                dense: true,
                leading: const Icon(Icons.school_outlined),
                title: Text(entries[i].label),
                selected: _selected == i + 1,
                onTap: () => _select(i + 1),
              ),
          ],
        ),
      ),
      // t94：用 IndexedStack 保留各栏状态——切到课表再切回来，对话（消息 + 输入草稿）不能丢。
      body: IndexedStack(
        index: destination == null ? 0 : 1,
        children: <Widget>[
          _ChatPane(chatSettings: _chatSettings, secrets: _secrets, auth: _auth),
          if (destination == null)
            const SizedBox.shrink()
          else
            _CampusPane(entry: destination),
        ],
      ),
    );
  }

  /// 对话设置与设置页**共享**同一个状态源（t86：一处改，两处立即见）
  late final ChatSettingsController _chatSettings = ChatSettingsController();

  /// 共享的服务实例（t89）：ChatPage 与 SettingsPage 用**同一份**，不是各自 new。
  /// 构造失败不影响启动：降级为 null（设置页里账号/Key 显示为暂不可用）。
  SecretService? _secrets;
  AuthService? _auth;

  void _initSharedServices() {
    try {
      _secrets = SecretService(dbPath: AppPaths.databasePath);
      _auth = AuthService();
    } on Object {
      _secrets = null;
      _auth = null;
    }
  }

  void _openSettings() {
    Navigator.of(context).push(
      MaterialPageRoute<void>(
        builder: (BuildContext context) =>
            SettingsPage(
              controller: widget.themeController,
              chatSettings: _chatSettings,
              authService: _auth,
              secretService: _secrets,
            ),
      ),
    );
  }

  void _select(int index) {
    setState(() => _selected = index);
    Navigator.of(context).maybePop();
  }
}

/// 对话主区：真正的流式对话（`ChatPage`）。
class _ChatPane extends StatelessWidget {
  const _ChatPane({required this.chatSettings, this.secrets, this.auth});

  final ChatSettingsController chatSettings;
  final SecretService? secrets;
  final AuthService? auth;

  @override
  Widget build(BuildContext context) =>
      ChatPage(
        dbPath: AppPaths.databasePath,
        chatSettings: chatSettings,
        secretService: secrets,
        authService: auth,
      );
}

/// 校务入口（后续替换为真实 WebView；Cookie 只留在本机，ADR-11）。
class _CampusPane extends StatelessWidget {
  const _CampusPane({required this.entry});

  final CampusEntry entry;

  /// 课表服务：进程内复用一个实例（核心库只加载一次）。
  static final TimetableService _service = TimetableService();

  /// 校务取数：WebView 版（Cookie 只留本机）。
  static final SchoolFetcher _fetcher = WebViewSchoolFetcher();

  @override
  Widget build(BuildContext context) {
    // 课表入口：真实链路（WebView 抓取 / 调试导入 → Rust 解析 → SQLite → 列表）
    if (entry == CampusEntry.timetable) {
      return TimetablePage(service: _service, fetcher: _fetcher);
    }
    // t52 P0-2：其余入口显示**自己的占位页**（带 URL），绝不能落到对话页
    return _EntryPlaceholder(entry: entry);
  }
}

/// 校务入口占位页（WebView 接入前的兜底；**绝不显示对话页**）。
class _EntryPlaceholder extends StatelessWidget {
  const _EntryPlaceholder({required this.entry});

  final CampusEntry entry;

  @override
  Widget build(BuildContext context) {
    return Center(
      child: Padding(
        padding: const EdgeInsets.all(24),
        child: Column(
          mainAxisAlignment: MainAxisAlignment.center,
          children: <Widget>[
            const Icon(Icons.public, size: 56),
            const SizedBox(height: 12),
            Text(entry.label, style: Theme.of(context).textTheme.titleMedium),
            const SizedBox(height: 8),
            SelectableText(entry.url, style: const TextStyle(fontSize: 12)),
            const SizedBox(height: 16),
            const Text(
              '该入口暂为占位页（后续在此内嵌 WebView）',
              textAlign: TextAlign.center,
            ),
          ],
        ),
      ),
    );
  }
}

/// 抽屉里的分组小标题（overline 层：10px + 字距）。
class _DrawerSectionLabel extends StatelessWidget {
  const _DrawerSectionLabel({required this.text});

  final String text;

  @override
  Widget build(BuildContext context) {
    final YunongTokens t = context.tokens;
    return Padding(
      padding: const EdgeInsets.fromLTRB(18, 14, 18, 4),
      child: Text(
        text,
        style: Theme.of(context).textTheme.labelSmall?.copyWith(color: t.inkSoft),
      ),
    );
  }
}