// t172：编码修复（标题栏）+ 删标题行 + rail 48 + composer 重构的验收。

import 'dart:io';

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

void main() {
  group('编码：标题栏不能乱码（t172 #1）', () {
    test('main.cpp 里没有裸中文字面量，只用 UTF-16 转义', () {
      final String source = File('windows/runner/main.cpp').readAsStringSync();
      // 文档注释/普通注释里的中文允许，但**字符串字面量**里不允许出现非 ASCII
      final RegExp literal = RegExp(r'L?"[^"\n]*"');
      final List<String> bad = <String>[];
      for (final RegExpMatch match in literal.allMatches(source)) {
        final String text = match.group(0)!;
        if (RegExp(r'[\u4e00-\u9fff]').hasMatch(text)) {
          bad.add(text);
        }
      }
      expect(bad, isEmpty, reason: '发现裸中文字符串字面量（MSVC 会按本地代码页解释）：$bad');
      // 必须使用转义写法
      expect(source, contains(r'L"\u4E91\u519C\u4E4B\u7A97"'));
    });

    test('Runner.rc 含中文 → 必须声明 UTF-8 代码页', () {
      final String rc = File('windows/runner/Runner.rc').readAsStringSync();
      final bool hasChinese = RegExp(r'[\u4e00-\u9fff]').hasMatch(rc);
      if (hasChinese) {
        expect(rc, contains('#pragma code_page(65001)'),
            reason: '含中文资源字符串时必须声明代码页，否则 rc.exe 按本地代码页读会乱码');
      }
    });
  });

  group('顶部只剩一行状态行（t172 #2）', () {
    testWidgets('AppBar 无标题文字，高度 44', (WidgetTester tester) async {
      await _pumpApp(tester, const Size(1280, 800));
      expect(find.byKey(const Key('app-title-main')), findsNothing);
      expect(find.byKey(const Key('app-title-sub')), findsNothing);
      expect(find.descendant(of: find.byType(AppBar), matching: find.byType(Text)), findsNothing);
      final AppBar appBar = tester.widget<AppBar>(find.byType(AppBar));
      expect(appBar.toolbarHeight, 44);
    });
  });

  group('rail 48（t172 #3）', () {
    testWidgets('rail 宽度实际渲染 = 48（窄屏）', (WidgetTester tester) async {
      await _pumpApp(tester, const Size(360, 800));
      expect(tester.getSize(find.byKey(const Key('app-sidebar'))).width, closeTo(48, 1));
    });

    for (final Size size in const <Size>[Size(360, 800), Size(1280, 800)]) {
      testWidgets('${size.width.toInt()}：汉堡与设置 dx 偏差 <1dp', (WidgetTester tester) async {
        await _pumpApp(tester, size);
        if (size.width < 600) {
          expect(tester.getSize(find.byKey(const Key('app-sidebar'))).width, closeTo(48, 1));
        }
        final double toggle = tester.getCenter(find.byKey(const Key('sidebar-toggle'))).dx;
        final double settings = tester.getCenter(find.byKey(const Key('sidebar-settings'))).dx;
        expect(toggle, closeTo(settings, 1));
      });
    }

    testWidgets('1280 展开态：侧栏仍 224，dx 偏差 <1dp', (WidgetTester tester) async {
      await _pumpApp(tester, const Size(1280, 800));
      expect(tester.getSize(find.byKey(const Key('app-sidebar'))).width, closeTo(224, 1));
      final double toggle = tester.getCenter(find.byKey(const Key('sidebar-toggle'))).dx;
      final double settings = tester.getCenter(find.byKey(const Key('sidebar-settings'))).dx;
      expect(toggle, closeTo(settings, 1));
    });

    testWidgets('窄屏内容左让 48', (WidgetTester tester) async {
      await _pumpApp(tester, const Size(360, 800));
      final Rect content = tester.getRect(find.byType(IndexedStack).first);
      expect(content.left, greaterThanOrEqualTo(48));
    });
  });
}