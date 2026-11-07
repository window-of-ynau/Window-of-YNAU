// t96：构建标识 + 文本选择菜单实心（右键菜单玻璃透底）。

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/main.dart';
import 'package:yunong/src/core/build_info.dart';
import 'package:yunong/src/settings/settings_page.dart';
import 'package:yunong/src/theme/context_menu.dart';
import 'package:yunong/src/theme/theme_controller.dart';

void main() {
  group('P0-1 构建标识', () {
    test('未注入时显示 dev（本地 flutter run 场景）', () {
      // 测试环境没有 --dart-define → 两者都为 dev
      expect(BuildInfo.commit, 'dev');
      expect(BuildInfo.time, 'dev');
      expect(BuildInfo.isRelease, isFalse);
      expect(BuildInfo.label, '构建 dev');
    });

    testWidgets('设置页底部显示版本 + 构建标识', (WidgetTester tester) async {
      final theme = ThemeController(store: MemorySettingsStore());
      await tester.pumpWidget(MaterialApp(
        home: SettingsPage(controller: theme, chatSettings: null),
      ));
      await tester.pumpAndSettle();

    // t115：设置页新增「供应商」卡后，关于组在更下方——先滚到底再断言
    await tester.scrollUntilVisible(
      find.textContaining('云农之窗', skipOffstage: false),
      200,
      scrollable: find.byType(Scrollable).first,
    );
    await tester.pumpAndSettle();
      expect(find.textContaining('云农之窗', skipOffstage: false), findsOneWidget);
      expect(find.textContaining('构建 dev', skipOffstage: false), findsOneWidget);
    });
  });

  group('P0-2 右键/长按选择菜单实心', () {
    test('三套主题的浮层 surface 都被强制为不透明', () {
      for (final AppThemeId id in AppThemeId.values) {
        for (final Brightness brightness in <Brightness>[Brightness.light, Brightness.dark]) {
          final ThemeData solid = solidSurfaceTheme(ThemeControllerTheme.of(id, brightness));
          expect(solid.colorScheme.surface.a, 1.0, reason: '$id/$brightness surface 必须不透明');
          expect(solid.colorScheme.surfaceContainerHighest.a, 1.0, reason: '$id/$brightness 容器浮层必须不透明');
        }
      }
    });

    testWidgets('App 子树被套上不透明 Theme（工具栏继承它）', (WidgetTester tester) async {
      tester.view.physicalSize = const Size(1200, 900);
      tester.view.devicePixelRatio = 1;
      addTearDown(tester.view.resetPhysicalSize);
      addTearDown(tester.view.resetDevicePixelRatio);

      await tester.pumpWidget(YunongApp(themeController: ThemeController(store: MemorySettingsStore())));
      await tester.pumpAndSettle();

      // 找 App 子树里那个不透明 surface 的 Theme（builder 注入的）
      final bool hasOpaqueTheme = tester
          .widgetList<Theme>(find.byType(Theme))
          .any((Theme t) => t.data.colorScheme.surface.a == 1.0);
      expect(hasOpaqueTheme, isTrue, reason: '文本选择工具栏必须继承不透明 surface');
    });
  });
}

/// 便捷：用主题控制器构造某主题的 ThemeData（测试里避免直接依赖 buildTheme 签名）。
abstract final class ThemeControllerTheme {
  static ThemeData of(AppThemeId id, Brightness brightness) =>
      ThemeController(store: MemorySettingsStore(), initialTheme: id).theme(brightness);
}
