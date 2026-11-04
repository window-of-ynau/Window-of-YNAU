// t117：侧栏 B 方案（可折叠 rail）——宽度、持久化、窄窗不溢出。

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/main.dart';
import 'package:yunong/src/theme/theme_controller.dart';

const Key _sidebar = Key('app-sidebar');
const Key _toggle = Key('sidebar-toggle');

MemorySettingsStore _store() => MemorySettingsStore();

double _width(WidgetTester tester) => tester.getSize(find.byKey(_sidebar)).width;

Future<void> _pumpApp(WidgetTester tester, {required Size size, required SettingsStore store}) async {
  await tester.binding.setSurfaceSize(size);
  addTearDown(() => tester.binding.setSurfaceSize(null));
  await tester.pumpWidget(YunongApp(themeController: ThemeController(store: store)));
  await tester.pumpAndSettle();
}

void main() {
  testWidgets('宽窗默认展开 >= 240', (WidgetTester tester) async {
    await _pumpApp(tester, size: const Size(1280, 800), store: _store());
    expect(_width(tester), greaterThanOrEqualTo(240));
    expect(find.text('对话'), findsWidgets);
  });

  testWidgets('点折叠 → 宽度 <= 72；再点展开 → 回到 >= 240', (WidgetTester tester) async {
    await _pumpApp(tester, size: const Size(1280, 800), store: _store());
    await tester.tap(find.byKey(_toggle));
    await tester.pumpAndSettle();
    expect(_width(tester), lessThanOrEqualTo(72));
    // rail 态：文字标签消失，图标仍在；设置入口仍可用
    expect(find.byKey(const Key('sidebar-settings')), findsOneWidget);

    await tester.tap(find.byKey(_toggle));
    await tester.pumpAndSettle();
    expect(_width(tester), greaterThanOrEqualTo(240));
  });

  testWidgets('折叠状态持久化：用同一 store 重建 App 仍是折叠态', (WidgetTester tester) async {
    final SettingsStore store = _store();
    await _pumpApp(tester, size: const Size(1280, 800), store: store);
    await tester.tap(find.byKey(_toggle));
    await tester.pumpAndSettle();
    expect(_width(tester), lessThanOrEqualTo(72));

    // 重建：新的 widget 树 + 同一个 store（模拟重启）
    await tester.pumpWidget(const SizedBox.shrink());
    await tester.pumpAndSettle();
    await tester.pumpWidget(YunongApp(themeController: ThemeController(store: store)));
    await tester.pumpAndSettle();
    expect(_width(tester), lessThanOrEqualTo(72), reason: '重启后应保持折叠');
  });

  testWidgets('480px 窄窗：未手动设置时自动折叠且不溢出', (WidgetTester tester) async {
    await _pumpApp(tester, size: const Size(480, 900), store: _store());
    expect(tester.takeException(), isNull);
    expect(_width(tester), lessThanOrEqualTo(72), reason: '窄窗应默认折叠为 rail');
  });
}
