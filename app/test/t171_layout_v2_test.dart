// t171：布局 v2 验收 —— 标题、两栏分界、左栏 224/200/64、图标列对齐 x=32、窄屏 rail 64。

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/main.dart';
import 'package:yunong/src/theme/theme_controller.dart';
import 'package:yunong/src/theme/yunong_tokens.dart';

import 'support/test_window.dart';

Future<void> _pumpApp(WidgetTester tester, Size size, {AppThemeId theme = AppThemeId.paperCoral}) async {
  setTestWindow(tester, size.width, size.height);
  await tester.pumpWidget(YunongApp(
    themeController: ThemeController(store: MemorySettingsStore(), initialTheme: theme),
  ));
  await tester.pumpAndSettle();
}

double _dx(WidgetTester tester, String key) =>
    tester.getCenter(find.byKey(Key(key))).dx;

void main() {
  group('标题（t171 #1）', () {
    testWidgets('宽屏：主标题「云农之窗」+ 副标题当前页名', (WidgetTester tester) async {
      await _pumpApp(tester, const Size(1280, 800));
      expect(find.byKey(const Key('app-title-main')), findsOneWidget);
      expect(tester.widget<Text>(find.byKey(const Key('app-title-main'))).data, '云农之窗');
      expect(find.byKey(const Key('app-title-sub')), findsOneWidget);
      expect(tester.widget<Text>(find.byKey(const Key('app-title-sub'))).data, '对话');
    });

    testWidgets('窄屏：只留主标题（无副标题）', (WidgetTester tester) async {
      await _pumpApp(tester, const Size(360, 800));
      expect(tester.widget<Text>(find.byKey(const Key('app-title-main'))).data, '云农之窗');
      expect(find.byKey(const Key('app-title-sub')), findsNothing);
    });

    testWidgets('侧栏底部不再有那条 Divider（owner 要求删除）', (WidgetTester tester) async {
      await _pumpApp(tester, const Size(1280, 800));
      final Finder dividers = find.descendant(
        of: find.byKey(const Key('app-sidebar')),
        matching: find.byType(Divider),
      );
      expect(dividers, findsNothing);
    });
  });

  group('两栏分界（t171 #2）', () {
    for (final AppThemeId id in AppThemeId.values) {
      testWidgets('${id.label}：分界线用 tokens.hairline', (WidgetTester tester) async {
        await _pumpApp(tester, const Size(1280, 800), theme: id);
        final BuildContext context = tester.element(find.byKey(const Key('app-sidebar')));
        final YunongTokens tokens = context.tokens;
        // 找那条 1px 线：宽为 1 且有颜色的 Container
        final Iterable<Container> containers = tester.widgetList<Container>(find.byType(Container));
        final Container line = containers.firstWhere(
          (Container c) =>
              c.constraints?.maxWidth == 1 ||
              (c.constraints?.minWidth == 1 && c.constraints?.maxWidth == 1),
          orElse: () => throw StateError('没找到 1px 分界线'),
        );
        expect(line.color, tokens.hairline, reason: '${id.label} 分界线颜色应为 hairline');
      });
    }
  });

  group('左栏宽度（t171 #3）', () {
    testWidgets('1280 宽：侧栏 224 ±1', (WidgetTester tester) async {
      await _pumpApp(tester, const Size(1280, 800));
      expect(tester.getSize(find.byKey(const Key('app-sidebar'))).width, closeTo(224, 1));
    });

    testWidgets('700 宽：侧栏 200 ±1', (WidgetTester tester) async {
      await _pumpApp(tester, const Size(700, 800));
      expect(tester.getSize(find.byKey(const Key('app-sidebar'))).width, closeTo(200, 1));
    });
  });

  group('图标列对齐 x=32（t171 #4）', () {
    testWidgets('窄屏：汉堡与底部设置同一条竖线', (WidgetTester tester) async {
      await _pumpApp(tester, const Size(360, 800));
      expect(_dx(tester, 'sidebar-toggle'), closeTo(_dx(tester, 'sidebar-settings'), 1));
    });

    testWidgets('宽屏折叠态：同一条竖线', (WidgetTester tester) async {
      await _pumpApp(tester, const Size(1280, 800));
      await tester.tap(find.byKey(const Key('sidebar-toggle')));
      await tester.pumpAndSettle();
      expect(tester.getSize(find.byKey(const Key('app-sidebar'))).width, lessThanOrEqualTo(72));
      expect(_dx(tester, 'sidebar-toggle'), closeTo(_dx(tester, 'sidebar-settings'), 1));
    });

    testWidgets('宽屏展开态：同一条竖线', (WidgetTester tester) async {
      await _pumpApp(tester, const Size(1280, 800));
      expect(tester.getSize(find.byKey(const Key('app-sidebar'))).width, closeTo(224, 1));
      expect(_dx(tester, 'sidebar-toggle'), closeTo(_dx(tester, 'sidebar-settings'), 1));
    });
  });

  group('窄屏内容不被遮挡（t171）', () {
    testWidgets('360×800：内容左边界 ≥ 64', (WidgetTester tester) async {
      await _pumpApp(tester, const Size(360, 800));
      final Rect content = tester.getRect(find.byType(IndexedStack).first);
      expect(content.left, greaterThanOrEqualTo(64));
    });
  });
}