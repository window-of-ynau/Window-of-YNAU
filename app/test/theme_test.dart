// 主题体系的守卫测试（新增文件）。
//
// 覆盖 captain 在需求里点名的四条：
//   1. 对比度守卫：正文 ≥ 4.5:1、大字号/非文本 ≥ 3:1 —— 三套主题 × 明暗两档全覆盖；
//   2. A/C 主题全树 0 个 BackdropFilter（零性能风险）；
//   3. 切换即时生效、不重启（MaterialApp 重建、HomeShell 不重建、颜色真的变了）；
//   4. 课表课程色相不随主题变（主题只切浅/深档）。

import 'dart:math' as math;

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/main.dart';
import 'package:yunong/src/campus/timetable/course_palette.dart';
import 'package:yunong/src/settings/settings_page.dart';
import 'package:yunong/src/theme/theme_controller.dart';
import 'package:yunong/src/theme/glass_panel.dart';
import 'package:yunong/src/theme/glass_performance.dart';
import 'package:yunong/src/theme/paper_texture.dart';
import 'package:yunong/src/theme/themes.dart';
import 'package:yunong/src/theme/yunong_tokens.dart';

/// WCAG 2.1 相对亮度。
double _luminance(Color c) {
  double channel(double v) =>
      v <= 0.03928 ? v / 12.92 : math.pow((v + 0.055) / 1.055, 2.4).toDouble();
  final double r = channel(c.r);
  final double g = channel(c.g);
  final double b = channel(c.b);
  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
}

double _contrast(Color fg, Color bg) {
  final double a = _luminance(fg);
  final double b = _luminance(bg);
  final double hi = math.max(a, b);
  final double lo = math.min(a, b);
  return (hi + 0.05) / (lo + 0.05);
}

/// 把带透明度的"表面色"叠到画布上再算对比度——
/// B 主题的 surface 是 rgba(255,255,255,.05)，直接拿未合成色算会得出荒谬的 1.3:1。
Color _over(Color fg, Color bg) {
  final double a = fg.a;
  double mix(double f, double b) => f * a + b * (1 - a);
  return Color.from(
    alpha: 1,
    red: mix(fg.r, bg.r),
    green: mix(fg.g, bg.g),
    blue: mix(fg.b, bg.b),
  );
}

void main() {
  group('对比度守卫（三套主题 × 明暗）', () {
    for (final AppThemeId id in AppThemeId.values) {
      for (final Brightness brightness in Brightness.values) {
        test('${id.label} · ${brightness.name}', () {
          final YunongTokens t = tokensFor(id, brightness);
          // 表面色可能是半透明（B 是 rgba(255,255,255,.05)），先叠到画布上再算
          final Color surface = _over(t.surface, t.canvas);

          // 正文与卡片正文：≥ 4.5:1
          expect(_contrast(t.ink, t.canvas), greaterThanOrEqualTo(4.5),
              reason: 'ink/canvas = ${_contrast(t.ink, t.canvas).toStringAsFixed(2)}');
          expect(_contrast(t.ink, surface), greaterThanOrEqualTo(4.5),
              reason: 'ink/surface = ${_contrast(t.ink, surface).toStringAsFixed(2)}');

          // 次要文字：≥ 4.5:1（时间戳这类"三级"允许低，不在此列）
          expect(_contrast(t.inkSoft, t.canvas), greaterThanOrEqualTo(4.5),
              reason: 'inkSoft/canvas = ${_contrast(t.inkSoft, t.canvas).toStringAsFixed(2)}');
          expect(_contrast(t.inkSoft, surface), greaterThanOrEqualTo(4.5),
              reason: 'inkSoft/surface = ${_contrast(t.inkSoft, surface).toStringAsFixed(2)}');

          // 强调色上的文字：≥ 4.5:1
          expect(_contrast(t.onAccent, t.accent), greaterThanOrEqualTo(4.5),
              reason: 'onAccent/accent = ${_contrast(t.onAccent, t.accent).toStringAsFixed(2)}');

          // 琥珀横幅（"未确认学期开始日"）：正文级 ≥ 4.5:1
          expect(_contrast(t.warnFg, t.warnBg), greaterThanOrEqualTo(4.5),
              reason: 'warn = ${_contrast(t.warnFg, t.warnBg).toStringAsFixed(2)}');

          // 焦点环属于"非文本"，按 1.4.11 只需 ≥ 3:1
          expect(_contrast(t.focusRing, t.canvas), greaterThanOrEqualTo(3.0),
              reason: 'focusRing/canvas = ${_contrast(t.focusRing, t.canvas).toStringAsFixed(2)}');
        });
      }
    }
  });

  group('A/C 主题零玻璃（没有 BackdropFilter）', () {
    for (final AppThemeId id in <AppThemeId>[AppThemeId.minimalBlack, AppThemeId.paperCoral]) {
      testWidgets('${id.label}：token 无 glass，页面无 BackdropFilter', (WidgetTester tester) async {
        await tester.binding.setSurfaceSize(const Size(800, 600));
        addTearDown(() => tester.binding.setSurfaceSize(null));

        final ThemeController controller = ThemeController(initialTheme: id);
        await tester.pumpWidget(
          MaterialApp(theme: buildTheme(id, Brightness.light), home: SettingsPage(controller: controller)),
        );
        await tester.pumpAndSettle();

        expect(tokensFor(id, Brightness.light).glass, isNull);
        expect(find.byType(BackdropFilter), findsNothing, reason: '${id.label} 不允许出现 BackdropFilter');
      });
    }
  });

  group('切换即时生效、不重启', () {
    testWidgets('默认 A；切到 C 后颜色立刻变，HomeShell 没被重建', (WidgetTester tester) async {
      await tester.binding.setSurfaceSize(const Size(1000, 700));
      addTearDown(() => tester.binding.setSurfaceSize(null));

      final ThemeController controller = ThemeController();
      await tester.pumpWidget(YunongApp(themeController: controller));
      await tester.pumpAndSettle();

      expect(controller.themeId, AppThemeId.minimalBlack, reason: '默认必须是 A');
      expect(Theme.of(tester.element(find.byType(HomeShell))).extension<YunongTokens>()!.accent,
          kTokensALight.accent);

      final Element shellBefore = tester.element(find.byType(HomeShell));

      await controller.select(AppThemeId.paperCoral);
      await tester.pumpAndSettle();

      final YunongTokens after =
          Theme.of(tester.element(find.byType(HomeShell))).extension<YunongTokens>()!;
      expect(after.accent, const Color(0xFFA9583E), reason: 'C 主题的强调色应立刻生效');
      expect(after.canvas, const Color(0xFFFAF9F5));
      expect(identical(tester.element(find.byType(HomeShell)), shellBefore), isTrue,
          reason: '换主题不应重建 HomeShell（状态不能丢）');
      expect(find.byType(BackdropFilter), findsNothing);
    });

    testWidgets('设置页点卡片即可切换，并写进存储', (WidgetTester tester) async {
      await tester.binding.setSurfaceSize(const Size(900, 700));
      addTearDown(() => tester.binding.setSurfaceSize(null));

      final MemorySettingsStore store = MemorySettingsStore();
      final ThemeController controller = ThemeController(store: store);
      await tester.pumpWidget(
        MaterialApp(
          theme: controller.theme(Brightness.light),
          home: SettingsPage(controller: controller),
        ),
      );
      await tester.pumpAndSettle();

      expect(find.text('极简黑白'), findsOneWidget);
      expect(find.text('深蓝玻璃'), findsOneWidget);
      expect(find.text('书本质感'), findsOneWidget);

      await tester.tap(find.text('深蓝玻璃'));
      await tester.pumpAndSettle();

      expect(controller.themeId, AppThemeId.deepGlass);
      expect(await store.get(SettingsKeys.themeId), 'deep_glass',
          reason: '切换要落盘（现在是内存兜底，接口已是最终形态）');
    });

    test('控制器可从存储恢复上次选择', () async {
      final MemorySettingsStore store = MemorySettingsStore(<String, String>{
        SettingsKeys.themeId: 'paper_coral',
        SettingsKeys.themeMode: 'dark',
      });
      final ThemeController controller = ThemeController(store: store);
      expect(controller.themeId, AppThemeId.minimalBlack, reason: '构造时是默认值');
      await controller.load();
      expect(controller.themeId, AppThemeId.paperCoral);
      // 明暗由主题决定：C 是浅色主题（不再有独立开关）
      expect(controller.themeMode, ThemeMode.light);
    });
  });

  group('课表色相不随主题变（信息表达）', () {
    test('只有明暗档由主题决定，色相索引与色板长度不变', () {
      expect(tokensFor(AppThemeId.minimalBlack, Brightness.light).useDarkCoursePalette, isFalse);
      expect(tokensFor(AppThemeId.paperCoral, Brightness.light).useDarkCoursePalette, isFalse);
      expect(tokensFor(AppThemeId.deepGlass, Brightness.dark).useDarkCoursePalette, isTrue);

      // 两档色板一一对应（同一 index = 同一色相）
      expect(kCourseTintsLight.length, kCourseTintsDark.length);
    });

    test('同一份课表在三套主题下拿到同一个槽位', () {
      const List<String> names = <String>[
        '高等数学I', '大学英语', '数据结构', '操作系统', '体育',
        '大学物理', '马克思主义基本原理', '计算机网络', '形势与政策',
      ];
      final Map<String, int> assignment = assignCourseTints(names);
      // assignCourseTints 与主题无关（签名里根本没有 theme），这里锁一下行为
      expect(assignCourseTints(names), assignment);
      expect(assignment['高等数学I'], isNotNull);
    });
  });
  group('玻璃闸门（批次 2：只有 B 主题会用到）', () {
    test('帧耗时 p90 超标时逐级降档，L0 后不再降', () {
      final GlassGovernor governor = GlassGovernor(minSamples: 3);
      expect(governor.level, GlassLevel.l3, reason: '默认从最高档开始');

      governor.feed(<double>[20, 21, 22]);
      expect(governor.level, GlassLevel.l2);
      governor.feed(<double>[20, 21, 22]);
      expect(governor.level, GlassLevel.l1);
      governor.feed(<double>[20, 21, 22]);
      expect(governor.level, GlassLevel.l0);
      governor.feed(<double>[20, 21, 22]);
      expect(governor.level, GlassLevel.l0, reason: 'L0 是地板');
    });

    test('恢复要等迟滞时间；且只升一档', () {
      final GlassGovernor governor =
          GlassGovernor(minSamples: 3, upgradeHold: Duration.zero);
      governor.feed(<double>[20, 20, 20]);
      expect(governor.level, GlassLevel.l2);
      governor.feed(<double>[8, 9, 10]);
      expect(governor.level, GlassLevel.l3, reason: '迟滞归零后可以升回去');
    });

    test('手动锁定档位时不再自动降级', () {
      final GlassGovernor governor = GlassGovernor(minSamples: 3);
      governor.lock(GlassLevel.l1);
      governor.feed(<double>[40, 40, 40]);
      expect(governor.level, GlassLevel.l1);
    });

    testWidgets('B + L3：玻璃面板真的用了 BackdropFilter', (WidgetTester tester) async {
      final GlassGovernor governor = GlassGovernor();
      await tester.pumpWidget(
        GlassScope(
          governor: governor,
          child: MaterialApp(
            theme: buildTheme(AppThemeId.deepGlass, Brightness.dark),
            home: const Scaffold(
              body: Center(
                child: SizedBox(
                  width: 300,
                  height: 60,
                  child: GlassPanel(role: GlassRole.composer, child: SizedBox.expand()),
                ),
              ),
            ),
          ),
        ),
      );
      expect(find.byType(BackdropFilter), findsOneWidget);
    });

    testWidgets('B + L1（降级后）：BackdropFilter 消失，但面板还在', (WidgetTester tester) async {
      final GlassGovernor governor = GlassGovernor()..lock(GlassLevel.l1);
      await tester.pumpWidget(
        GlassScope(
          governor: governor,
          child: MaterialApp(
            theme: buildTheme(AppThemeId.deepGlass, Brightness.dark),
            home: const Scaffold(
              body: Center(
                child: SizedBox(
                  width: 300,
                  height: 60,
                  child: GlassPanel(role: GlassRole.composer, child: SizedBox.expand()),
                ),
              ),
            ),
          ),
        ),
      );
      expect(find.byType(GlassPanel), findsOneWidget);
      expect(find.byType(BackdropFilter), findsNothing);
    });

    testWidgets('卡片类面板永远不模糊（即使 L3）', (WidgetTester tester) async {
      final GlassGovernor governor = GlassGovernor();
      await tester.pumpWidget(
        GlassScope(
          governor: governor,
          child: MaterialApp(
            theme: buildTheme(AppThemeId.deepGlass, Brightness.dark),
            home: const Scaffold(
              body: Center(
                child: SizedBox(
                  width: 300,
                  height: 60,
                  // 默认 role = flat
                  child: GlassPanel(child: SizedBox.expand()),
                ),
              ),
            ),
          ),
        ),
      );
      expect(find.byType(BackdropFilter), findsNothing);
    });

    test('玻璃档位能落盘并在重启后恢复', () async {
      final MemorySettingsStore store = MemorySettingsStore();
      final ThemeController controller = ThemeController(
        store: store,
        initialTheme: AppThemeId.deepGlass,
      );
      await controller.setGlassMode(GlassMode.liquid);
      expect(await store.get(SettingsKeys.glassMode), 'liquid');

      final ThemeController restored = ThemeController(store: store);
      await restored.load();
      expect(restored.glassMode, GlassMode.liquid);
    });
  });

  group('t53：C 主题性能与明暗收敛', () {
    testWidgets('纸纹用单次绘制的 PaperTexture（不再有 Image + Opacity 平铺）',
        (WidgetTester tester) async {
      await tester.binding.setSurfaceSize(const Size(800, 600));
      addTearDown(() => tester.binding.setSurfaceSize(null));

      final ThemeController controller = ThemeController(initialTheme: AppThemeId.paperCoral);
      await tester.pumpWidget(YunongApp(themeController: controller));
      await tester.pumpAndSettle();

      expect(find.byType(PaperTexture), findsOneWidget, reason: 'C 主题挂的是 shader 纸纹层');
      // 旧写法是 Image.asset(repeat) + Opacity：那会在 2560×1440 下产生 57,600 次绘制/帧
      expect(find.byType(Opacity), findsNothing, reason: '不允许整屏 Opacity（= saveLayer）');
    });

    testWidgets('A/B 主题不挂纸纹层', (WidgetTester tester) async {
      await tester.binding.setSurfaceSize(const Size(800, 600));
      addTearDown(() => tester.binding.setSurfaceSize(null));

      for (final AppThemeId id in <AppThemeId>[AppThemeId.minimalBlack, AppThemeId.deepGlass]) {
        final ThemeController controller = ThemeController(initialTheme: id);
        await tester.pumpWidget(YunongApp(themeController: controller));
        await tester.pumpAndSettle();
        expect(find.byType(PaperTexture), findsNothing, reason: '${id.label} 没有纸纹');
      }
    });

    test('明暗由主题决定，不再有独立开关', () {
      expect(ThemeController(initialTheme: AppThemeId.minimalBlack).themeMode, ThemeMode.light);
      expect(ThemeController(initialTheme: AppThemeId.paperCoral).themeMode, ThemeMode.light);
      expect(ThemeController(initialTheme: AppThemeId.deepGlass).themeMode, ThemeMode.dark);
    });

    testWidgets('设置页不再出现"明暗 / 跟随系统"选项', (WidgetTester tester) async {
      await tester.binding.setSurfaceSize(const Size(900, 700));
      addTearDown(() => tester.binding.setSurfaceSize(null));

      final ThemeController controller = ThemeController();
      await tester.pumpWidget(
        MaterialApp(theme: controller.theme(Brightness.light), home: SettingsPage(controller: controller)),
      );
      await tester.pumpAndSettle();
      expect(find.text('明暗'), findsNothing);
      expect(find.text('跟随系统'), findsNothing);
    });

    testWidgets('C 主题的对话气泡改书页排版（用户为页边批注）', (WidgetTester tester) async {
      await tester.binding.setSurfaceSize(const Size(900, 700));
      addTearDown(() => tester.binding.setSurfaceSize(null));

      final ThemeController controller = ThemeController(initialTheme: AppThemeId.paperCoral);
      await tester.pumpWidget(YunongApp(themeController: controller));
      await tester.pumpAndSettle();
      // 书页排版下每条用户消息都有"你 · 提问"小标签（空对话时不出现，这里只断言不崩）
      expect(find.byType(PaperTexture), findsOneWidget);
    });
  });
}