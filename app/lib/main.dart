// 云农之窗 · 客户端骨架（W1）
//
// 形态已定（ADR-07 / T-02）：**Flutter 原生 UI**，只在打开校务系统时内嵌 WebView。
// 所以这里看到的是原生导航 + 原生对话区，而不是"整个 App 是一层网页壳"。
//
// 本文件只负责结构与导航；真正的对话、课表、日程在后续里程碑接入 Rust 核心（FFI）。

import 'package:flutter/material.dart';

import 'src/campus/timetable_page.dart';
import 'src/core/timetable_service.dart';

void main() => runApp(const YunongApp());

/// 校务入口（阶段一只做导航；登录与取数在 WebView 里完成，见 ADR-11）。
enum CampusEntry {
  portal('教务门户', 'https://jwxt.ynau.edu.cn/DefaultStu'),
  scores('成绩查询', 'https://jwgl.ynau.edu.cn/STU/cj_default.aspx'),
  timetable('我的课表', 'https://qzjwxt.ynau.edu.cn/jsxsd/framework/xsMainV.htmlx'),
  library('图书馆', 'http://10.8.225.101:8013/#/Home'),
  software('正版软件', 'http://software.ynau.edu.cn/');

  const CampusEntry(this.label, this.url);

  final String label;
  final String url;
}

class YunongApp extends StatelessWidget {
  const YunongApp({super.key});

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: '云农之窗',
      debugShowCheckedModeBanner: false,
      theme: ThemeData(
        colorScheme: ColorScheme.fromSeed(seedColor: const Color(0xFF2563EB)),
        useMaterial3: true,
      ),
      darkTheme: ThemeData(
        colorScheme: ColorScheme.fromSeed(
          seedColor: const Color(0xFF2563EB),
          brightness: Brightness.dark,
        ),
        useMaterial3: true,
      ),
      themeMode: ThemeMode.system,
      home: const HomeShell(),
    );
  }
}

/// 左侧导航 + 右侧主区（PRD §4 信息架构）。
class HomeShell extends StatefulWidget {
  const HomeShell({super.key});

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
      appBar: AppBar(
        title: Text(destination?.label ?? '云农之窗'),
        actions: <Widget>[
          IconButton(
            tooltip: '设置',
            onPressed: () => _showPlaceholder(context, '设置', 'BYOK / 套餐、记忆与隐私选项将在 W2 接入'),
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

  void _select(int index) {
    setState(() => _selected = index);
    Navigator.of(context).maybePop();
  }

  void _showPlaceholder(BuildContext context, String title, String message) {
    showDialog<void>(
      context: context,
      builder: (BuildContext ctx) => AlertDialog(
        title: Text(title),
        content: Text(message),
        actions: <Widget>[
          TextButton(onPressed: () => Navigator.of(ctx).pop(), child: const Text('知道了')),
        ],
      ),
    );
  }
}

/// 对话主区（占位）。
class _ChatPane extends StatelessWidget {
  const _ChatPane();

  @override
  Widget build(BuildContext context) {
    return Center(
      child: Padding(
        padding: const EdgeInsets.all(24),
        child: Column(
          mainAxisAlignment: MainAxisAlignment.center,
          children: <Widget>[
            const Icon(Icons.window, size: 64),
            const SizedBox(height: 16),
            Text('你好，我是云农之窗', style: Theme.of(context).textTheme.titleLarge),
            const SizedBox(height: 8),
            const Text('问课表、加日程、查规章 —— 一句话就行', textAlign: TextAlign.center),
            const SizedBox(height: 24),
            const Text(
              'W1 骨架：对话与流式将在 W2 接 Rust 核心',
              style: TextStyle(fontSize: 12, color: Colors.grey),
            ),
          ],
        ),
      ),
    );
  }
}

/// 校务入口（后续替换为真实 WebView；Cookie 只留在本机，ADR-11）。
class _CampusPane extends StatelessWidget {
  const _CampusPane({required this.entry});

  final CampusEntry entry;

  /// 课表服务：进程内复用一个实例（核心库只加载一次）。
  static final TimetableService _service = TimetableService();

  @override
  Widget build(BuildContext context) {
    // 课表入口：真实链路（WebView 抓取 / 调试导入 → Rust 解析 → SQLite → 列表）
    if (entry == CampusEntry.timetable) {
      return TimetablePage(service: _service);
    }
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
