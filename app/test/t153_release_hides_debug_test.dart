// t153：v0.9 alpha 前收口 —— release 构建（模拟）里调试/诊断入口不出现，debug 里仍在。

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/src/campus/session_probe.dart';
import 'package:yunong/src/chat/chat_settings_section.dart';
import 'package:yunong/src/chat/chat_top_bar.dart';
import 'package:yunong/src/core/debug_flags.dart';
import 'package:yunong/src/settings/settings_page.dart';
import 'package:yunong/src/theme/theme_controller.dart';

import 'support/test_window.dart';

Widget _settings({required bool? showDebugEntries}) => MaterialApp(
      home: SettingsPage(
        controller: ThemeController(store: MemorySettingsStore()),
        chatSettings: ChatSettingsController(),
        showDebugEntries: showDebugEntries,
      ),
    );

Widget _topBar({required bool? showDebugEntries, VoidCallback? onDiagnostics}) => MaterialApp(
      home: Scaffold(
        body: ChatTopBar(
          keyDegraded: false,
          signedIn: false,
          accountLabel: '',
          statusLabel: '就绪',
          searchLabel: '未配置',
          keyLabel: '未设置',
          onLocalSearch: () {},
          onMemory: () {},
          onDiagnostics: onDiagnostics ?? () {},
          onInfo: () {},
          onClearConversation: () {},
          channel: 'byok',
          onChannelChanged: (String _) {},
          showDebugEntries: showDebugEntries,
        ),
      ),
    );

void main() {
  tearDown(DebugFlags.reset);

  group('release（模拟）：调试入口隐藏', () {
    testWidgets('设置页不含「[临时] 会话校验探针」', (WidgetTester tester) async {
      setTestWindow(tester, 1280, 900);
      await tester.pumpWidget(_settings(showDebugEntries: false));
      await tester.pumpAndSettle();
      // 滚动到底（「关于」分组在最后）
      await tester.scrollUntilVisible(
        find.text('关于'),
        300,
        scrollable: find.byType(Scrollable).first,
      );
      expect(find.byKey(const Key('settings-session-probe')), findsNothing);
      expect(find.text('[临时] 会话校验探针'), findsNothing);
      // 版本行仍在（只隐藏调试入口，不动正常内容）
      expect(find.text('版本'), findsOneWidget);
    });

    testWidgets('对话页 ⋮ 里不含「对话诊断」', (WidgetTester tester) async {
      setTestWindow(tester, 1280, 900);
      await tester.pumpWidget(_topBar(showDebugEntries: false));
      await tester.pumpAndSettle();
      await tester.tap(find.byIcon(Icons.more_vert));
      await tester.pumpAndSettle();
      expect(find.text('对话诊断'), findsNothing);
      // 其它菜单项仍在
      expect(find.text('清空当前对话历史'), findsOneWidget);
      expect(find.text('记忆'), findsOneWidget);
    });

    testWidgets('DebugFlags.override=false 时默认即隐藏（构建模式开关生效）', (WidgetTester tester) async {
      DebugFlags.override = false;
      setTestWindow(tester, 1280, 900);
      await tester.pumpWidget(_settings(showDebugEntries: null));
      await tester.pumpAndSettle();
      await tester.scrollUntilVisible(
        find.text('关于'),
        300,
        scrollable: find.byType(Scrollable).first,
      );
      expect(find.text('[临时] 会话校验探针'), findsNothing);
    });
  });

  group('debug：入口仍在', () {
    testWidgets('设置页有「[临时] 会话校验探针」', (WidgetTester tester) async {
      setTestWindow(tester, 1280, 900);
      await tester.pumpWidget(_settings(showDebugEntries: true));
      await tester.pumpAndSettle();
      await tester.scrollUntilVisible(
        find.byKey(const Key('settings-session-probe')),
        300,
        scrollable: find.byType(Scrollable).first,
      );
      expect(find.byKey(const Key('settings-session-probe')), findsOneWidget);
      expect(find.text('[临时] 会话校验探针'), findsOneWidget);
    });

    testWidgets('对话页 ⋮ 里有「对话诊断」且可打开', (WidgetTester tester) async {
      setTestWindow(tester, 1280, 900);
      int opened = 0;
      await tester.pumpWidget(_topBar(showDebugEntries: true, onDiagnostics: () => opened += 1));
      await tester.pumpAndSettle();
      await tester.tap(find.byIcon(Icons.more_vert));
      await tester.pumpAndSettle();
      expect(find.text('对话诊断'), findsOneWidget);
      await tester.tap(find.text('对话诊断'));
      await tester.pumpAndSettle();
      expect(opened, 1);
    });
  });

  test('DebugFlags：默认跟随构建模式，override 可模拟 release', () {
    DebugFlags.reset();
    expect(DebugFlags.showDebugEntries, isTrue, reason: '测试跑在 debug 下');
    DebugFlags.override = false;
    expect(DebugFlags.showDebugEntries, isFalse);
    DebugFlags.override = true;
    expect(DebugFlags.showDebugEntries, isTrue);
    DebugFlags.reset();
    expect(DebugFlags.showDebugEntries, isTrue);
  });

  test('功能必需的东西不在开关范围内（避免误删）', () {
    // isCampusHost 硬守卫：与调试可见性无关，永远生效
    expect(isCampusHost('one.ynau.edu.cn'), isTrue);
    expect(isCampusHost('example.com'), isFalse);
  });
}
