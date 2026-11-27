// t6：Android 分层导航（#90）+ 导航图标（#84）实现验收。
//
// 三档断点（design/android-adaptive-nav.md §9）：
//   手机 <600：无常驻侧栏，汉堡在状态行左侧，点开全高抽屉；
//   平板 600–839：常驻 rail 64；
//   桌面 ≥840：侧栏 224。
// 图标：#84 每个入口一枚独立 20dp 图标，默认 inkSoft / 选中 ink。

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/main.dart';
import 'package:yunong/src/campus/campus_entries.dart';
import 'package:yunong/src/theme/theme_controller.dart';
import 'package:yunong/src/theme/yunong_tokens.dart';

import 'support/test_window.dart';

Future<void> _pumpApp(WidgetTester tester, Size size) async {
  setTestWindow(tester, size.width, size.height);
  await tester.pumpWidget(YunongApp(
    themeController: ThemeController(store: MemorySettingsStore()),
  ));
  await tester.pumpAndSettle();
}

YunongTokens _tokensOf(WidgetTester tester, Finder anchor) =>
    tester.element(anchor).tokens;

Icon _iconIn(WidgetTester tester, Finder container, IconData data) =>
    tester.widget<Icon>(find.descendant(of: container, matching: find.byIcon(data)));

void main() {
  group('#90 手机 <600：无 rail + 抽屉', () {
    testWidgets('360×800：无常驻侧栏；汉堡在状态行（y<60）；点开抽屉', (WidgetTester tester) async {
      await _pumpApp(tester, const Size(360, 800));

      // 1) 没有任何常驻侧栏 / rail
      expect(find.byKey(const Key('app-sidebar')), findsNothing, reason: '手机档不得有常驻 rail');
      // 2) 汉堡在状态行（顶部 44 内，y<60）
      final Finder toggle = find.byKey(const Key('sidebar-toggle'));
      expect(toggle, findsOneWidget);
      expect(tester.getRect(toggle).top, lessThan(60));

      // 3) 点汉堡 → 抽屉出现，宽度 = min(320, 360-56) = 304
      await tester.tap(toggle);
      await tester.pumpAndSettle();
      expect(find.byKey(const Key('nav-drawer')), findsOneWidget);
      expect(tester.getSize(find.byKey(const Key('nav-drawer'))).width, closeTo(304, 1));

      // 4) 遮罩 32% 黑
      final Scaffold scaffold = tester.widget<Scaffold>(find.byType(Scaffold).first);
      expect(scaffold.drawerScrimColor, const Color(0x52000000));

      // 5) 行高 44 + 选中态（默认「对话」）
      expect(tester.getRect(find.byKey(const Key('drawer-item-chat'))).height, closeTo(44, 0.5));
      final YunongTokens tokens = _tokensOf(tester, find.byKey(const Key('nav-drawer')));
      expect(_iconIn(tester, find.byKey(const Key('drawer-item-chat')), Icons.chat_bubble_outline).color,
          tokens.ink,
          reason: '选中项图标应为 ink');
    });

    testWidgets('360×800：抽屉里点「成绩查询」→ 选中态切换、页面切换', (WidgetTester tester) async {
      await _pumpApp(tester, const Size(360, 800));
      await tester.tap(find.byKey(const Key('sidebar-toggle')));
      await tester.pumpAndSettle();

      await tester.tap(find.byKey(const Key('drawer-item-scores')));
      await tester.pumpAndSettle();
      // 切到成绩查询占位页
      expect(find.text('成绩查询'), findsWidgets);

      // 重开抽屉：成绩 = ink（选中），对话 = inkSoft（未选中）
      await tester.tap(find.byKey(const Key('sidebar-toggle')));
      await tester.pumpAndSettle();
      final YunongTokens tokens = _tokensOf(tester, find.byKey(const Key('nav-drawer')));
      expect(_iconIn(tester, find.byKey(const Key('drawer-item-scores')), Icons.assessment_outlined).color,
          tokens.ink);
      expect(_iconIn(tester, find.byKey(const Key('drawer-item-chat')), Icons.chat_bubble_outline).color,
          tokens.inkSoft);
    });
  });

  group('#90 平板 600–839：rail 64', () {
    testWidgets('700×800：常驻 rail 64，无抽屉；入口图标独立且 20dp', (WidgetTester tester) async {
      await _pumpApp(tester, const Size(700, 800));
      expect(find.byKey(const Key('app-sidebar')), findsOneWidget);
      expect(tester.getSize(find.byKey(const Key('app-sidebar'))).width, closeTo(64, 1));
      expect(find.byKey(const Key('nav-drawer')), findsNothing, reason: '平板档不用抽屉');

      final Finder sidebar = find.byKey(const Key('app-sidebar'));
      final Set<IconData> icons = tester
          .widgetList<Icon>(find.descendant(of: sidebar, matching: find.byType(Icon)))
          .map((Icon i) => i.icon)
          .whereType<IconData>()
          .toSet();
      for (final CampusEntry entry in CampusEntry.values) {
        expect(icons, contains(entry.icon), reason: '缺少 ${entry.label} 的独立图标');
      }
      expect(icons.length, greaterThanOrEqualTo(7), reason: '5 入口 + 对话 + 设置（+汉堡）应各不相同');
      // 20dp 图标
      expect(tester.getSize(find.byIcon(CampusEntry.timetable.icon)).width, closeTo(20, 0.5));
    });
  });

  group('#90 桌面 ≥840：侧栏 224', () {
    testWidgets('1280×800：侧栏 224；图标独立、选中项 ink', (WidgetTester tester) async {
      await _pumpApp(tester, const Size(1280, 800));
      expect(tester.getSize(find.byKey(const Key('app-sidebar'))).width, closeTo(224, 1));

      final Finder sidebar = find.byKey(const Key('app-sidebar'));
      final Set<IconData> icons = tester
          .widgetList<Icon>(find.descendant(of: sidebar, matching: find.byType(Icon)))
          .map((Icon i) => i.icon)
          .whereType<IconData>()
          .toSet();
      for (final CampusEntry entry in CampusEntry.values) {
        expect(icons, contains(entry.icon));
      }
      expect(icons.length, greaterThanOrEqualTo(6));

      final YunongTokens tokens = _tokensOf(tester, sidebar);
      expect(_iconIn(tester, sidebar, Icons.chat_bubble_outline).color, tokens.ink,
          reason: '默认选中「对话」，图标应为 ink');
      expect(_iconIn(tester, sidebar, CampusEntry.scores.icon).color, tokens.inkSoft);
    });
  });
}
