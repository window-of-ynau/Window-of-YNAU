// t112：UI 五项（⋮ 菜单 / 设置入口在抽屉底部 / 无顶栏齿轮）。
//
// 说明：设置页「模型与凭据」与 Term 重载另有覆盖（timetable_page_test 与
// t89_settings_final_test）；这里集中验证**入口与菜单**的形态。

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/main.dart';
import 'package:yunong/src/chat/chat_top_bar.dart';

Widget _host(Widget child) => MaterialApp(home: Scaffold(body: child));

void main() {
  group('⋮ 菜单（t112 第一/二项）', () {
    Future<void> openMenu(WidgetTester tester) async {
      await tester.pumpWidget(_host(ChatTopBar(
        channel: 'byok',
        onChannelChanged: (_) {},
        statusLabel: '就绪',
        searchLabel: '未配置',
        keyLabel: 'sk-****cdef',
        keyDegraded: false,
        signedIn: true,
        accountLabel: 'a***@b.com',
        onLocalSearch: () {},
        onMemory: () {},
        onDiagnostics: () {},
        onClearConversation: () {},
      )));
      await tester.pumpAndSettle();
      await tester.tap(find.byIcon(Icons.more_vert));
      await tester.pumpAndSettle();
    }

    testWidgets('只剩四项：搜索本地资料 / 记忆 / 对话诊断 / 清空当前对话历史', (WidgetTester tester) async {
      await openMenu(tester);
      expect(find.text('搜索本地资料（不联网）'), findsOneWidget);
      expect(find.text('记忆'), findsOneWidget);
      expect(find.text('对话诊断'), findsOneWidget);
      expect(find.text('清空当前对话历史'), findsOneWidget);
      // Key / 账号项已移到设置页
      expect(find.text('删除已保存的 Key'), findsNothing);
      expect(find.textContaining('登录套餐账号'), findsNothing);
      expect(find.textContaining('退出套餐'), findsNothing);
    });
  });

  group('设置入口在抽屉底部（t112 第五项）', () {
    testWidgets('顶栏无齿轮；抽屉底部有整行「设置」', (WidgetTester tester) async {
      await tester.pumpWidget(const YunongApp());
      await tester.pumpAndSettle();

      // 顶栏齿轮已移除
      expect(find.byTooltip('设置'), findsNothing);

      // t117：侧栏改为常驻可折叠（不再有抽屉），设置入口在侧栏底部

      final Finder settingsTile = find.byKey(const Key('sidebar-settings'));
      expect(settingsTile, findsOneWidget);
      expect(find.descendant(of: settingsTile, matching: find.text('设置')), findsOneWidget);

      // 整行撑满：宽度接近侧栏宽度
      final Size sidebarSize = tester.getSize(find.byKey(const Key('app-sidebar')));
      final Size tileSize = tester.getSize(settingsTile);
      expect(tileSize.width, greaterThan(sidebarSize.width * 0.9));
    });
  });
}
