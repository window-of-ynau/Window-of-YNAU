// t165：手机端两问题 —— 窄屏布局（内容不被 rail 压住）+ 端侧核心状态可显示。

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/main.dart';
import 'package:yunong/src/chat/chat_settings_section.dart';
import 'package:yunong/src/core/ffi_bindings.dart';
import 'package:yunong/src/settings/settings_page.dart';
import 'package:yunong/src/theme/theme_controller.dart';

import 'support/test_window.dart';

Future<void> _pumpApp(WidgetTester tester, Size size) async {
  setTestWindow(tester, size.width, size.height);
  await tester.pumpWidget(YunongApp(
    themeController: ThemeController(store: MemorySettingsStore()),
  ));
  await tester.pumpAndSettle();
}

void main() {
  tearDown(() => CoreStatus.debugLatest = null);

  group('手机（<600）布局：无侧栏 + 全宽内容（t6 #90）', () {
    for (final Size size in const <Size>[Size(360, 800), Size(412, 915)]) {
      testWidgets('${size.width.toInt()}×${size.height.toInt()}：无 rail、内容全宽、汉堡在状态行',
          (WidgetTester tester) async {
        await _pumpApp(tester, size);

        // 1) 手机档没有任何常驻侧栏
        expect(find.byKey(const Key('app-sidebar')), findsNothing);
        // 2) 不再有底部导航
        expect(find.byKey(const Key('app-bottom-nav')), findsNothing);
        // 3) 内容全宽（不再左让 48）
        final Rect content = tester.getRect(find.byType(IndexedStack).first);
        expect(content.left, lessThan(1), reason: '手机内容应全宽，实际 ${content.left}');
        // 4) 汉堡在状态行（y<60）
        expect(tester.getRect(find.byKey(const Key('sidebar-toggle'))).top, lessThan(60));
        // 5) t172：AppBar 不再有标题行
        expect(find.byKey(const Key('app-title-main')), findsNothing);
      });
    }
  });

  group('中/宽屏（t6 #90）：平板 rail 64 / 桌面 224', () {
    testWidgets('700 宽：平板 rail 64 与内容并排', (WidgetTester tester) async {
      await _pumpApp(tester, const Size(700, 800));
      final Rect rail = tester.getRect(find.byKey(const Key('app-sidebar')));
      final Rect content = tester.getRect(find.byType(IndexedStack).first);
      expect(rail.width, closeTo(64, 1), reason: '平板应为 rail 64');
      expect(content.left, greaterThanOrEqualTo(rail.right));
    });

    testWidgets('1280 宽：宽侧栏 224 与内容并排', (WidgetTester tester) async {
      await _pumpApp(tester, const Size(1280, 800));
      final Rect sidebar = tester.getRect(find.byKey(const Key('app-sidebar')));
      final Rect content = tester.getRect(find.byType(IndexedStack).first);
      expect(sidebar.width, closeTo(224, 1), reason: '宽屏应为 224');
      expect(content.left, greaterThanOrEqualTo(sidebar.right));
    });
  });

  group('端侧核心状态（t165）', () {
    test('成功：显示大小（yunong_core 6.85 MB）', () {
      const CoreStatus ok = CoreStatus.testOnly(
        ok: true,
        detail: '已就绪（yunong_core 0.9.1）',
        sizeBytes: 7182745,
        version: '0.9.1',
      );
      expect(ok.label, contains('已就绪'));
      expect(ok.label, contains('MB'));
    });

    test('失败：把尝试过的路径与错误写出来', () {
      const CoreStatus bad = CoreStatus.testOnly(
        ok: false,
        detail: '未加载（尝试过：libyunong_core.so；错误：libyunong_core.so: cannot open shared object file）',
      );
      expect(bad.label, startsWith('端侧核心：未加载'));
      expect(bad.label, contains('尝试过'));
      expect(bad.label, contains('cannot open shared object file'));
    });

    testWidgets('设置页「关于」常驻一行核心状态（失败态也显示原因）',
        (WidgetTester tester) async {
      CoreStatus.debugLatest = const CoreStatus.testOnly(
        ok: false,
        detail: '未加载（尝试过：libyunong_core.so；错误：dlopen failed）',
      );
      setTestWindow(tester, 1280, 1000);
      await tester.pumpWidget(MaterialApp(
        home: SettingsPage(
          controller: ThemeController(store: MemorySettingsStore()),
          chatSettings: ChatSettingsController(),
        ),
      ));
      await tester.pumpAndSettle();
      await tester.scrollUntilVisible(
        find.byKey(const Key('settings-core-status')),
        300,
        scrollable: find.byType(Scrollable).first,
      );
      expect(find.text('端侧核心'), findsOneWidget);
      expect(find.textContaining('未加载'), findsOneWidget);
      expect(find.textContaining('dlopen failed'), findsOneWidget);
    });
  });
}
