// t153（t161 精简）：release 构建隐藏「对话诊断」，debug 仍可见。
//
// 会话校验探针已整体下线（t161 / issue #55），这里只剩对话诊断这一项。

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/src/campus/campus_guard.dart';
import 'package:yunong/src/chat/chat_top_bar.dart';
import 'package:yunong/src/core/debug_flags.dart';

import 'support/test_window.dart';

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

  group('release（模拟）：诊断入口隐藏', () {
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

    testWidgets('DebugFlags.override=false 时默认即隐藏（构建模式开关生效）',
        (WidgetTester tester) async {
      DebugFlags.override = false;
      setTestWindow(tester, 1280, 900);
      await tester.pumpWidget(_topBar(showDebugEntries: null));
      await tester.pumpAndSettle();
      await tester.tap(find.byIcon(Icons.more_vert));
      await tester.pumpAndSettle();
      expect(find.text('对话诊断'), findsNothing);
    });
  });

  group('debug：入口仍在', () {
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

  test('功能必需的东西不受影响（校方域名硬守卫）', () {
    expect(isCampusHost('one.ynau.edu.cn'), isTrue);
    expect(isCampusHost('ssoo.ynau.edu.cn'), isTrue);
    expect(isCampusHost('example.com'), isFalse);
  });
}
