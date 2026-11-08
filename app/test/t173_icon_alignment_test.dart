// t173：汉堡与侧栏设置**像素级同一条竖线**（x = 24），三态各测一次。

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/main.dart';
import 'package:yunong/src/theme/theme_controller.dart';

import 'support/test_window.dart';

Future<void> _pumpApp(WidgetTester tester, Size size) async {
  setTestWindow(tester, size.width, size.height);
  await tester.pumpWidget(YunongApp(
    themeController: ThemeController(store: MemorySettingsStore()),
  ));
  await tester.pumpAndSettle();
}

double _dx(WidgetTester tester, String key) =>
    tester.getCenter(find.byKey(Key(key))).dx;

void main() {
  group('图标列像素级对齐 x=24（t173）', () {
    testWidgets('窄屏 360：toggle 与 settings 都是 24', (WidgetTester tester) async {
      await _pumpApp(tester, const Size(360, 800));
      final double toggle = _dx(tester, 'sidebar-toggle');
      final double settings = _dx(tester, 'sidebar-settings');
      expect(toggle, closeTo(24, 0.5), reason: 'toggle=$toggle');
      expect(settings, closeTo(24, 0.5), reason: 'settings=$settings');
      expect(toggle, closeTo(settings, 0.5));
    });

    testWidgets('宽屏 1280 展开：两者都是 24', (WidgetTester tester) async {
      await _pumpApp(tester, const Size(1280, 800));
      final double toggle = _dx(tester, 'sidebar-toggle');
      final double settings = _dx(tester, 'sidebar-settings');
      expect(tester.getSize(find.byKey(const Key('app-sidebar'))).width, closeTo(224, 1));
      expect(toggle, closeTo(24, 0.5), reason: 'toggle=$toggle');
      expect(settings, closeTo(24, 0.5), reason: 'settings=$settings');
      expect(toggle, closeTo(settings, 0.5));
    });

    testWidgets('宽屏 1280 折叠：两者都是 24', (WidgetTester tester) async {
      await _pumpApp(tester, const Size(1280, 800));
      await tester.tap(find.byKey(const Key('sidebar-toggle')));
      await tester.pumpAndSettle();
      final double toggle = _dx(tester, 'sidebar-toggle');
      final double settings = _dx(tester, 'sidebar-settings');
      expect(tester.getSize(find.byKey(const Key('app-sidebar'))).width, closeTo(48, 1));
      expect(toggle, closeTo(24, 0.5), reason: 'toggle=$toggle');
      expect(settings, closeTo(24, 0.5), reason: 'settings=$settings');
      expect(toggle, closeTo(settings, 0.5));
    });

    testWidgets('AppBar 不再自带 leading 内边距（automaticallyImplyLeading=false）',
        (WidgetTester tester) async {
      await _pumpApp(tester, const Size(1280, 800));
      final AppBar appBar = tester.widget<AppBar>(find.byType(AppBar));
      expect(appBar.automaticallyImplyLeading, isFalse);
      expect(appBar.leadingWidth, 48);
    });
  });
}