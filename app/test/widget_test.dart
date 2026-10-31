import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/main.dart';

void main() {
  testWidgets('启动后能看到对话主区', (WidgetTester tester) async {
    await tester.pumpWidget(const YunongApp());
    expect(find.text('你好，我是云农之窗'), findsOneWidget);
  });

  testWidgets('侧栏能进入校务入口', (WidgetTester tester) async {
    await tester.pumpWidget(const YunongApp());
    await tester.tap(find.byTooltip('Open navigation menu'));
    await tester.pumpAndSettle();
    await tester.tap(find.text('我的课表'));
    await tester.pumpAndSettle();
    expect(find.text('W2 将在此内嵌 WebView'), findsNothing);
    expect(find.textContaining('内嵌 WebView'), findsOneWidget);
  });
}
