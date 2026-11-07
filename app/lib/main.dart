import 'dart:async';
import 'dart:io';

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
import 'src/core/ffi_bindings.dart';
import 'src/core/titlebar_service.dart';
import 'src/chat/chat_settings_section.dart';
import 'src/core/auth_service.dart';
import 'src/core/secret_service.dart';
import 'src/theme/context_menu.dart';
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

void main(List<String> args) {
  WidgetsFlutterBinding.ensureInitialized();
  // t156：数据根覆盖（命令行 > 环境变量 > 默认）。
  //
  // 为什么两处都收：Flutter Windows 的 `main(List<String> args)` 在 release 里通常拿不到
  // 启动参数，`Platform.executableArguments` 才是可靠的；`args` 只是测试/调试时的补充。
  AppPaths.initDataRoot(
    args: <String>[...Platform.executableArguments, ...args],
  );
  final String? dataRootError = AppPaths.dataRootError;
  if (dataRootError != null) {
    // **明确报错并拒绝启动业务**：绝不能静默回退默认目录（否则又会复用本地数据库）
    runApp(DataRootErrorApp(
      message: dataRootError,
      path: AppPaths.dataDir,
      sourceLabel: AppPaths.overrideSource.label,
    ));
    return;
  }
  // t165：启动时探一次端侧核心（只 dlopen + 读版本，不建表），
  // 结果供设置页「关于」常驻显示：已就绪（大小）/ 未加载（具体路径与错误）。
  try {
    CoreStatus.probe();
  } on Object {
    // 探测本身失败不应阻止启动（横幅与设置页会显示"未加载"）
  }
  runApp(const YunongApp());
}

/// 数据目录非法时的兜底界面：只说明问题与解决方式，不进入业务（避免写错目录还不自知）。
class DataRootErrorApp extends StatelessWidget {
  const DataRootErrorApp({
    super.key,
    required this.message,
    required this.path,
    required this.sourceLabel,
  });

  final String message;
  final String path;
  final String sourceLabel;

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      debugShowCheckedModeBanner: false,
      home: Scaffold(
        body: Center(
          child: ConstrainedBox(
            constraints: const BoxConstraints(maxWidth: 560),
            child: Padding(
              padding: const EdgeInsets.all(24),
              child: Column(
                mainAxisSize: MainAxisSize.min,
                crossAxisAlignment: CrossAxisAlignment.start,
                children: <Widget>[
                  const Text('数据目录不可用',
                      style: TextStyle(fontSize: 20, fontWeight: FontWeight.bold)),
                  const SizedBox(height: 12),
                  SelectableText(message, key: const Key('data-root-error')),
                  const SizedBox(height: 8),
                  SelectableText('数据目录（$sourceLabel）：$path',
                      style: const TextStyle(fontSize: 12)),
                  const SizedBox(height: 16),
                  const Text('请确认路径存在且可写，或用 --data-dir 指定其它目录后重新启动。',
                      style: TextStyle(fontSize: 12)),
                ],
              ),
            ),
          ),
        ),
      ),
    );
  }
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
            // t96：右键/长按的文本选择菜单必须实心（glass 主题下默认会透底）
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
              // t96：文本选择菜单/浮层必须不透明（glass 主题下默认会透底），这里统一抬高 surface alpha
              Widget content = child ?? const SizedBox.shrink();
              content = Theme(
                key: const Key('app-solid-surface'),
                data: solidSurfaceTheme(Theme.of(context)),
                child: content,
              );
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
    // t117：恢复侧栏折叠状态（窄窗未手动设置时默认折叠）
    unawaited(_loadSidebarState());
  }

  int _selected = 0;

  /// t117：侧栏折叠状态。
  ///
  /// `null` = 用户从未手动设置过 → 窄窗（< 600）自动折叠；手动设置过则一直尊重它。
  bool? _collapsedOverride;
  static const String _collapsedKey = 'sidebar.collapsed';
  static const double kSidebarWidth = 256;
  static const double kRailWidth = 64;
  static const double kPhoneBreakpoint = 600;
  static const double kWideBreakpoint = 840;

  /// 兼容旧名字（t117 起的窄窗断点）
  static const double kNarrowBreakpoint = kPhoneBreakpoint;

  @override
  Widget build(BuildContext context) {
    const entries = CampusEntry.values;
    final destination = _selected == 0 ? null : entries[_selected - 1];
    final double width = MediaQuery.sizeOf(context).width;
    // t165：三档布局 —— 手机（<600）用底部导航；中宽（600–840）用窄 rail；
    // 宽屏（≥840）保持「侧栏 + 内容」。手机档**完全不放侧栏**，内容自然不会被压住。
    final bool phone = width < kPhoneBreakpoint;
    final bool middle = !phone && width < kWideBreakpoint;
    final bool collapsed = _collapsedOverride ?? middle;

    final Widget panes = IndexedStack(
      index: destination == null ? 0 : 1,
      children: <Widget>[
        _ChatPane(chatSettings: _chatSettings, secrets: _secrets, auth: _auth),
        if (destination == null) const SizedBox.shrink() else _CampusPane(entry: destination),
      ],
    );

    final Widget? sidebar = phone
        ? null
        : _Sidebar(
            collapsed: collapsed,
            selected: _selected,
            entries: entries,
            onSelect: _select,
            onToggle: _toggleSidebar,
            onOpenSettings: _openSettings,
          );

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
        // 手机档没有侧栏，也就不需要折叠按钮（省下左侧 48dp 给标题）
        leading: phone
            ? null
            : IconButton(
                key: const Key('sidebar-toggle'),
                tooltip: collapsed ? '展开侧栏' : '折叠侧栏',
                onPressed: _toggleSidebar,
                icon: Icon(collapsed ? Icons.menu : Icons.menu_open),
              ),
        title: Text(destination?.label ?? '云农之窗'),
      ),
      // 手机档：底部导航（内容占满整宽，绝不被遮挡）
      bottomNavigationBar: phone
          ? NavigationBar(
              key: const Key('app-bottom-nav'),
              selectedIndex: _selected,
              labelBehavior: NavigationDestinationLabelBehavior.alwaysShow,
              onDestinationSelected: _select,
              destinations: <Widget>[
                const NavigationDestination(
                  icon: Icon(Icons.chat_bubble_outline),
                  selectedIcon: Icon(Icons.chat_bubble),
                  label: '对话',
                ),
                for (final CampusEntry entry in entries)
                  NavigationDestination(
                    icon: const Icon(Icons.school_outlined),
                    selectedIcon: const Icon(Icons.school),
                    label: entry.label,
                  ),
              ],
            )
          : null,
      body: phone
          ? panes
          // 中/宽屏：侧栏与内容并排（正常文档流），内容左边界 = 侧栏右边界
          : Row(
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: <Widget>[sidebar!, const VerticalDivider(width: 1), Expanded(child: panes)],
            ),
    );
  }

  void _toggleSidebar() {
    final bool next = !(_collapsedOverride ??
        (MediaQuery.sizeOf(context).width < kNarrowBreakpoint));
    setState(() => _collapsedOverride = next);
    // 持久化：与主题同级（settings.json）
    unawaited(widget.themeController.store.set(_collapsedKey, next ? 'true' : 'false'));
  }

  Future<void> _loadSidebarState() async {
    try {
      final String? raw = await widget.themeController.store.get(_collapsedKey);
      if (!mounted || raw == null) {
        return;
      }
      setState(() => _collapsedOverride = raw == 'true');
    } on Object {
      // 读不到就用默认（宽窗展开 / 窄窗自动折叠），不影响启动。
    }
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
/// 可折叠侧栏（t117，B 方案）。
///
/// 展开 256 / 折叠 64（rail 只留图标 + tooltip），用 `AnimatedContainer` 做平滑过渡。
/// 底部「设置」在两种形态下都可用（rail 态是带 tooltip 的图标按钮）。
class _Sidebar extends StatelessWidget {
  const _Sidebar({
    required this.collapsed,
    required this.selected,
    required this.entries,
    required this.onSelect,
    required this.onToggle,
    required this.onOpenSettings,
  });

  final bool collapsed;
  final int selected;
  final List<CampusEntry> entries;
  final ValueChanged<int> onSelect;
  final VoidCallback onToggle;
  final VoidCallback onOpenSettings;

  @override
  Widget build(BuildContext context) {

    return AnimatedContainer(
      key: const Key('app-sidebar'),
      duration: const Duration(milliseconds: 180),
      curve: Curves.easeOutCubic,
      width: collapsed ? _HomeShellState.kRailWidth : _HomeShellState.kSidebarWidth,
      // 宽度动画期间内容始终按**当前目标宽度**布局再裁切，避免 ListTile 在中间宽度触发横向溢出。
      //
      // t132（阻塞一）根因：这里曾经固定把内容按 256 布局、rail 态（容器 64）再裁切 ——
      // `IconButton` 在 256 宽度里**居中**，图标落在 x≈128，被 64px 裁切区切掉，
      // 视觉上只剩一个纯色块（与主题无关，所以换主题也一样）。
      // 现在 rail 态内容按 64 布局，图标回到可视区中央。
      child: ClipRect(
        child: OverflowBox(
          alignment: Alignment.centerLeft,
          minWidth: collapsed ? _HomeShellState.kRailWidth : _HomeShellState.kSidebarWidth,
          maxWidth: collapsed ? _HomeShellState.kRailWidth : _HomeShellState.kSidebarWidth,
          child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: <Widget>[
          // t123：侧栏头部不再重复应用名，也不再放第二个「<」——顶栏汉堡就是唯一的折叠开关。

          Expanded(
            child: ListView(
              padding: EdgeInsets.zero,
              children: <Widget>[
                if (!collapsed) const _DrawerSectionLabel(text: '功能'),
                _SidebarItem(
                  collapsed: collapsed,
                  icon: Icons.chat_bubble_outline,
                  label: '对话',
                  selected: selected == 0,
                  onTap: () => onSelect(0),
                ),
                if (!collapsed) _DrawerSectionLabel(text: '校园 · ${entries.length}'),
                for (var i = 0; i < entries.length; i++)
                  _SidebarItem(
                    collapsed: collapsed,
                    icon: Icons.school_outlined,
                    label: entries[i].label,
                    selected: selected == i + 1,
                    onTap: () => onSelect(i + 1),
                  ),
              ],
            ),
          ),
          const Divider(height: 1),
          // 底部固定：rail 态是图标按钮（tooltip），展开态是整行 ListTile
          if (collapsed)
            IconButton(
              key: const Key('sidebar-settings'),
              tooltip: '设置',
              onPressed: onOpenSettings,
              icon: const Icon(Icons.settings_outlined),
            )
          else
            ListTile(
              key: const Key('sidebar-settings'),
              dense: true,
              leading: const Icon(Icons.settings_outlined),
              title: const Text('设置'),
              onTap: onOpenSettings,
            ),
          ],
        ),
      ),
    ),
    );
  }
}

/// 侧栏的一项：展开是 ListTile，折叠是带 tooltip 的图标。
class _SidebarItem extends StatelessWidget {
  const _SidebarItem({
    required this.collapsed,
    required this.icon,
    required this.label,
    required this.selected,
    required this.onTap,
  });

  final bool collapsed;
  final IconData icon;
  final String label;
  final bool selected;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    if (collapsed) {
      return Tooltip(
        message: label,
        child: IconButton(
          onPressed: onTap,
          icon: Icon(icon),
          isSelected: selected,
        ),
      );
    }
    return ListTile(
      dense: true,
      leading: Icon(icon),
      title: Text(label),
      selected: selected,
      onTap: onTap,
    );
  }
}
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