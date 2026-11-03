// t102：工具调用在对话界面的呈现（事件解析 + 卡片 + 折叠 + 失败状态）。
//
// 注意：Rust 侧尚未把 tool_call/tool_result 经 FFI 透出（已提案），
// 这里用**真实的事件 JSON 形状**测解析与渲染，不伪造"已接通"。

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/src/chat/tool_call_card.dart';
import 'package:yunong/src/core/chat_service.dart';

void main() {
  group('事件解析（真实 JSON 形状）', () {
    test('tool_call：解析 name / args / call_id', () {
      final e = ChatEvent.fromJsonString(
        '{"type":"tool_call","call_id":"c1","name":"timetable_query","args":{"week":8}}',
      );
      expect(e.kind, ChatEventKind.toolCall);
      expect(e.toolCallId, 'c1');
      expect(e.toolName, 'timetable_query');
      expect(e.toolArgs, contains('"week":8'));
    });

    test('tool_result 成功：ok + summary', () {
      final e = ChatEvent.fromJsonString(
        '{"type":"tool_result","call_id":"c1","ok":true,"summary":"5 条结果"}',
      );
      expect(e.kind, ChatEventKind.toolResult);
      expect(e.toolOk, isTrue);
      expect(e.toolSummary, '5 条结果');
    });

    test('tool_result 失败：ok=false + 原因（不静默）', () {
      final e = ChatEvent.fromJsonString(
        '{"type":"tool_result","call_id":"c1","ok":false,"summary":"超时 10s","code":"TIMEOUT"}',
      );
      expect(e.toolOk, isFalse);
      expect(e.errorCode, 'TIMEOUT');
      expect(e.toolSummary, contains('超时'));
    });
  });

  group('ToolCallCard 渲染', () {
    Future<void> pump(WidgetTester tester, Widget card) async {
      await tester.pumpWidget(MaterialApp(
        home: Scaffold(body: Center(child: SizedBox(width: 420, child: card))),
      ));
      await tester.pumpAndSettle();
    }

    testWidgets('标题是中文别名 + 参数提示；参数默认收起', (WidgetTester tester) async {
      await pump(
        tester,
        const ToolCallCard(
          toolName: 'timetable_query',
          status: ToolCallStatus.running,
          args: '{"week": 8}',
        ),
      );

      expect(find.textContaining('调用 课表查询(week=8)'), findsOneWidget);
      expect(find.text('进行中…'), findsOneWidget);
      // 默认收起：参数原文不可见
      expect(find.textContaining('"week": 8'), findsNothing);
    });

    testWidgets('展开后看到参数原文', (WidgetTester tester) async {
      await pump(
        tester,
        const ToolCallCard(
          toolName: 'timetable_query',
          status: ToolCallStatus.ok,
          args: '{"week": 8}',
          summary: '5 条结果',
        ),
      );
      expect(find.text('5 条结果'), findsOneWidget);

      await tester.tap(find.textContaining('调用 课表查询'));
      await tester.pumpAndSettle();
      expect(find.textContaining('"week": 8'), findsWidgets);
    });

    testWidgets('失败状态显式：显示「失败」+ 原因，不用静默', (WidgetTester tester) async {
      await pump(
        tester,
        const ToolCallCard(
          toolName: 'score_query',
          status: ToolCallStatus.failed,
          args: '{"term":"2026-1"}',
          errorText: '工具执行超时（10s）',
        ),
      );

      expect(find.text('失败'), findsOneWidget);
      expect(find.text('工具执行超时（10s）'), findsOneWidget);
    });
  });
}
