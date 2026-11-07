// t149：课表页的右键/长按菜单必须不透明（三主题）。
//
// 根因：菜单（文本选择工具栏 / showMenu / PopupMenu）读的是**页面 context 的主题**，
// 而 `colorScheme.*` 之外的浮层字段（`canvasColor` / `cardColor` / `popupMenuTheme.color` /
// `menuTheme`）一直是半透明或透明的 —— 课表页「学期」输入框正好会弹这个菜单，于是透出课程块。
// 修法：`solidSurfaceTheme` 把这些字段一并 solid 化；课表页再用 `OpaqueMenuScope` 兜住本页子树。

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/src/campus/timetable_page.dart';
import 'package:yunong/src/core/ffi_bindings.dart';
import 'package:yunong/src/core/timetable_service.dart';
import 'package:yunong/src/theme/context_menu.dart';
import 'package:yunong/src/theme/theme_controller.dart';

import 'support/test_window.dart';

ThemeData _base(AppThemeId id) =>
    ThemeController(store: MemorySettingsStore(), initialTheme: id).theme(Brightness.light);

void main() {
  for (final AppThemeId id in AppThemeId.values) {
    test('${id.label}：solidSurfaceTheme 后菜单相关颜色全部不透明', () {
      final ThemeData solid = solidSurfaceTheme(_base(id));
      final Color? menu = solid.menuTheme.style?.backgroundColor?.resolve(const <WidgetState>{});
      final Color? popup = solid.popupMenuTheme.color;
      expect(solid.canvasColor.a, 1.0, reason: '${id.label} canvasColor 透明会让菜单透底');
      expect(solid.cardColor.a, 1.0);
      expect(solid.dialogTheme.backgroundColor?.a, 1.0);
      expect(solid.colorScheme.surfaceContainerHigh.a, 1.0);
      expect(popup?.a, 1.0, reason: '${id.label} PopupMenu 背景必须不透明');
      expect(menu?.a, 1.0, reason: '${id.label} MenuAnchor 背景必须不透明');
    });

    testWidgets('${id.label}：课表页子树套了 OpaqueMenuScope（页面内弹菜单不会透）',
        (WidgetTester tester) async {
      setTestWindow(tester, 1280, 800);
      // 故意**不**在 MaterialApp.builder 里套全局 solid 主题：
      // 只用页面自身的 OpaqueMenuScope 兜底，验证"页面级"这条防线独立成立。
      await tester.pumpWidget(MaterialApp(
        theme: _base(id),
        home: Builder(
          builder: (BuildContext context) => Scaffold(
            body: TimetablePage(
              service: TimetableService(core: _NoopCore()),
            ),
          ),
        ),
      ));
      await tester.pumpAndSettle();

      // 页面 must 有 OpaqueMenuScope
      expect(find.byType(OpaqueMenuScope), findsWidgets,
          reason: '${id.label} 课表页必须用 OpaqueMenuScope 包裹');

      // 读**作用域之内**的 context（学期输入框在 OpaqueMenuScope 里；外层主题仍是带 alpha 的）
      final BuildContext pageContext = tester.element(find.byType(TextField).first);
      final ThemeData pageTheme = Theme.of(pageContext);
      expect(pageTheme.canvasColor.a, 1.0);
      expect(pageTheme.cardColor.a, 1.0);
      expect(pageTheme.colorScheme.surfaceContainerHigh.a, 1.0);
      expect(pageTheme.popupMenuTheme.color?.a, 1.0);
      expect(pageTheme.menuTheme.style?.backgroundColor?.resolve(const <WidgetState>{})?.a, 1.0);
    });
  }
}

/// 不触达真实 DLL 的核心替身：课表页 initState 会读本地课表，这里让它静默失败（页面仍可用）。
class _NoopCore implements CoreApi {
  @override
  dynamic noSuchMethod(Invocation invocation) => null;
}
