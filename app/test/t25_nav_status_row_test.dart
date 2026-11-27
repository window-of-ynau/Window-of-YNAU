// t12 F4/F5/F6：手机合并为一行状态栏 + 平板无死按钮 + 手机 ⚙ 快捷入口。

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/main.dart';
import 'package:yunong/src/chat/chat_top_bar.dart';
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
  testWidgets('360×800：只有一个状态行（ChatTopBar 44），无 _PhoneAppBar；汉堡与 ⚙ 都在这一行',
      (WidgetTester tester) async {
    await _pumpApp(tester, const Size(360, 800));

    expect(find.byKey(const Key('phone-app-bar')), findsNothing,
        reason: 'F4：不得再额外渲染一条 44dp 顶栏');
    expect(find.byType(ChatTopBar), findsOneWidget);
    final Rect bar = tester.getRect(find.byType(ChatTopBar));
    expect(bar.height, closeTo(44, 1), reason: '状态行应为 44dp，实际 ${bar.height}');
    expect(bar.top, lessThanOrEqualTo(1));

    final Finder chatBar = find.byType(ChatTopBar);
    expect(
      find.descendant(of: chatBar, matching: find.byKey(const Key('sidebar-toggle'))),
      findsOneWidget,
    );
    expect(
      find.descendant(of: chatBar, matching: find.byKey(const Key('phone-settings'))),
      findsOneWidget,
    );
  });

  testWidgets('360×800：汉堡可点开抽屉；切到校务页仍是同一行状态行', (WidgetTester tester) async {
    await _pumpApp(tester, const Size(360, 800));
    expect(find.byKey(const Key('app-sidebar')), findsNothing);

    await tester.tap(find.byKey(const Key('sidebar-toggle')));
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('nav-drawer')), findsOneWidget);

    await tester.tap(find.byKey(const Key('drawer-item-portal')));
    await tester.pumpAndSettle();
    expect(find.byType(ChatTopBar), findsNothing, reason: '校务页不应有对话页顶栏');
    final Rect header = tester.getRect(find.byKey(const Key('phone-campus-header')));
    expect(header.height, closeTo(44, 1));
    expect(
      find.descendant(
        of: find.byKey(const Key('phone-campus-header')),
        matching: find.byKey(const Key('sidebar-toggle')),
      ),
      findsOneWidget,
    );
    await tester.tap(find.byKey(const Key('sidebar-toggle')));
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('nav-drawer')), findsOneWidget);
  });

  testWidgets('360×800：状态行 ⚙ 直达设置页', (WidgetTester tester) async {
    await _pumpApp(tester, const Size(360, 800));
    await tester.tap(find.byKey(const Key('phone-settings')));
    await tester.pumpAndSettle();
    expect(find.byType(SettingsPage), findsOneWidget);
  });

  testWidgets('700×800：平板 rail 64，没有死汉堡按钮（也没有手机 ⚙）', (WidgetTester tester) async {
    await _pumpApp(tester, const Size(700, 800));
    expect(tester.getSize(find.byKey(const Key('app-sidebar'))).width, closeTo(64, 1));
    expect(find.byKey(const Key('sidebar-toggle')), findsNothing,
        reason: '平板常驻 rail 无折叠对象，不渲染汉堡');
    expect(find.byKey(const Key('phone-settings')), findsNothing);
  });
}
