import 'package:flutter/material.dart' show Icons;
import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/main.dart';

void main() {
  testWidgets('启动后能看到对话主区', (WidgetTester tester) async {
    await tester.pumpWidget(const YunongApp());
    expect(find.text('你好，我是云农之窗'), findsOneWidget);
  });

  testWidgets('侧栏能进入课表页', (WidgetTester tester) async {
    await tester.pumpWidget(const YunongApp());
    // 用图标而不是 tooltip 定位（tooltip 文案随 MaterialLocalizations 变，图标更稳）
    await tester.tap(find.byIcon(Icons.menu));
    await tester.pumpAndSettle();
    await tester.tap(find.text('我的课表'));
    await tester.pumpAndSettle();
    // W2b 起课表是真实页面（WebView 抓取 / 调试导入 → Rust 解析 → SQLite → 列表）。
    // 这里只断言入口按钮存在，避免把「外壳测试」绑死在页面内部文案上。
    expect(find.text('登录并抓取课表'), findsOneWidget);
  });
}
