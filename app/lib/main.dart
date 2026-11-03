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
  const YunongApp({super.key, this.themeController});

  /// 测试可注入；生产为 null（现造一个，内存兜底存储）。
  final ThemeController? themeController;

  @override
  State<YunongApp> createState() => _YunongAppState();
}

class _YunongAppState extends State<YunongApp> {
  late final ThemeController _controller =
      widget.themeController ?? ThemeController();

  /// 玻璃帧率闸门：**只在 B（深蓝玻璃）主题下挂载**，A/C 完全不启动。
  final GlassGovernor _glass = GlassGovernor();

  @override
  void initState() {
    super.initState();
    // 读回上次选择（默认 A 极简黑白）。存储未接时是内存实现，读不到就用默认值。
    unawaited(_controller.load());
    _controller.addListener(_syncGlassGovernor);
    _syncGlassGovernor();
  }

  void _syncGlassGovernor() {
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
            themeMode: _controller.themeMode,
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
        child: ListView(
          children: <Widget>[
            const DrawerHeader(
              child: Align(
                alignment: Alignment.bottomLeft,
                child: Text('云农之窗', style: TextStyle(fontSize: 22)),
              ),
            ),
            ListTile(
              leading: const Icon(Icons.chat_bubble_outline),
              title: const Text('对话'),
              selected: _selected == 0,
              onTap: () => _select(0),
            ),
            const Divider(),
            for (var i = 0; i < entries.length; i++)
              ListTile(
                leading: const Icon(Icons.school_outlined),
                title: Text(entries[i].label),
                selected: _selected == i + 1,
                onTap: () => _select(i + 1),
              ),
          ],
        ),
      ),
      body: destination == null
          ? const _ChatPane()
          : _CampusPane(entry: destination),
    );
  }

  void _openSettings() {
    Navigator.of(context).push(
      MaterialPageRoute<void>(
        builder: (BuildContext context) =>
            SettingsPage(controller: widget.themeController),
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
  const _ChatPane();

  @override
  Widget build(BuildContext context) => const ChatPage();
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
    return const ChatPage();
    // ignore: dead_code
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
              'W2 将在此内嵌 WebView：登录一次、Cookie 留本机、免二次登录',
              textAlign: TextAlign.center,
              style: TextStyle(fontSize: 12, color: Colors.grey),
            ),
          ],
        ),
      ),
    );
  }
}
