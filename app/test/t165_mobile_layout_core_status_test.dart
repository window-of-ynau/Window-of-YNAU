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

  group('窄屏（手机）布局：内容不被导航压住（t165）', () {
    for (final Size size in const <Size>[Size(360, 800), Size(412, 915)]) {
      testWidgets('${size.width.toInt()}×${size.height.toInt()}：用底部导航，无侧栏，内容占满整宽',
          (WidgetTester tester) async {
        await _pumpApp(tester, size);

        // 1) 手机档不该有侧栏（更不该有浮在内容上的 rail）
        expect(find.byKey(const Key('app-sidebar')), findsNothing);
        // 2) 改成底部导航
        expect(find.byKey(const Key('app-bottom-nav')), findsOneWidget);

        // 3) 内容区左边界贴到 0（AppBar/导航栏之外没有遮挡）
        final Rect content = tester.getRect(find.byType(IndexedStack).first);
        expect(content.left, lessThanOrEqualTo(0.5),
            reason: '内容区左边界应为 0，实际 ${content.left}');

        // 4) 关键文本完整可见、不被裁切：标题占满可用宽度
        final Finder title = find.text('云农之窗');
        expect(title, findsOneWidget);
        final Rect titleRect = tester.getRect(title);
        expect(titleRect.left, greaterThanOrEqualTo(0), reason: '标题左侧被裁');
        expect(titleRect.right, lessThanOrEqualTo(size.width), reason: '标题右侧出界');

        // 5) 底部导航不覆盖内容（内容底边 ≤ 导航栏顶边）
        final Rect nav = tester.getRect(find.byKey(const Key('app-bottom-nav')));
        expect(content.bottom, lessThanOrEqualTo(nav.top + 0.5),
            reason: '内容被底部导航覆盖：content.bottom=${content.bottom} nav.top=${nav.top}');
      });
    }
  });

  group('中/宽屏保持 rail / 侧栏（t165）', () {
    testWidgets('700 宽：窄 rail（64）+ 内容并排，内容左边界 = rail 右边界',
        (WidgetTester tester) async {
      await _pumpApp(tester, const Size(700, 800));
      expect(find.byKey(const Key('app-bottom-nav')), findsNothing);
      final Rect rail = tester.getRect(find.byKey(const Key('app-sidebar')));
      final Rect content = tester.getRect(find.byType(IndexedStack).first);
      expect(rail.width, lessThanOrEqualTo(72), reason: '中宽应为窄 rail');
      expect(content.left, greaterThanOrEqualTo(rail.right),
          reason: '内容左边界(${content.left}) 必须 ≥ rail 右边界(${rail.right})');
    });

    testWidgets('1280 宽：宽侧栏（256）与内容并排', (WidgetTester tester) async {
      await _pumpApp(tester, const Size(1280, 800));
      expect(find.byKey(const Key('app-bottom-nav')), findsNothing);
      final Rect sidebar = tester.getRect(find.byKey(const Key('app-sidebar')));
      final Rect content = tester.getRect(find.byType(IndexedStack).first);
      expect(sidebar.width, greaterThan(200), reason: '宽屏应为展开侧栏');
      expect(content.left, greaterThanOrEqualTo(sidebar.right));
    });
  });

  group('端侧核心状态（t165）', () {
    test('成功：显示大小（yunong_core 6.85 MB）', () {
      const CoreStatus ok = CoreStatus.testOnly(
        ok: true,
        detail: '已就绪（yunong_core 0.9.0）',
        sizeBytes: 7182745,
        version: '0.9.0',
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
