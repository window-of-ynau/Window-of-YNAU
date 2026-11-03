// t104：真实工具事件接线（db_path 启用 + call_id 配对 + 六个失败码 + 取消语义）。
//
// 事件样本来自 Rust 侧实测 wire（任务里贴出的原始 JSON），不是构造的假形状。

import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/src/chat/tool_call_card.dart';
import 'package:yunong/src/core/app_paths.dart';
import 'package:yunong/src/core/chat_service.dart';

void main() {
  // t106 实测修正：Rust 把 db_path 当**数据库文件**打开（传目录会 SQLITE_CANTOPEN）。
  group('db_path：启用内置工具的数据库文件路径', () {
    test('请求 JSON 里的 db_path 是 .db 文件路径（实测形态）', () {
      final String dbFile = '${Directory.systemTemp.path}/t104/app.db';
      final String dataDir = AppPaths.dataDirOf(dbFile);
      expect(dataDir, isNot(contains('app.db')));
      final ChatRequestDto request = ChatRequestDto(
        channel: ChatChannel.byok,
        model: 'deepseek-chat',
        messages: <ChatTurn>[ChatTurn.user('你好')],
        dbPath: dbFile,
      );
      final String json = request.toJson();
      expect(json, contains('"db_path"'));
      expect(json, contains(dataDir.replaceAll('\\', '\\\\')));
      expect(json.contains('app.db'), isTrue, reason: '实测：db_path 必须是数据库文件');
    });

    test('不传 db_path → 行为与旧版一致（不启用工具）', () {
      final ChatRequestDto request = ChatRequestDto(
        channel: ChatChannel.byok,
        model: 'deepseek-chat',
        messages: <ChatTurn>[ChatTurn.user('你好')],
      );
      expect(request.toJson(), isNot(contains('db_path')));
    });
  });

  group('真实 wire 解析（Rust 实测样本）', () {
    test('tool_call：args 是对象，call_id/name/index 都在', () {
      final e = ChatEvent.fromJsonString(
        '{"args":{"week":8},"call_id":"call_1","index":0,"name":"echo","type":"tool_call"}',
      );
      expect(e.kind, ChatEventKind.toolCall);
      expect(e.toolCallId, 'call_1');
      expect(e.toolName, 'echo');
      expect(e.toolArgs, contains('"week":8'));
    });

    test('tool_result 成功', () {
      final e = ChatEvent.fromJsonString(
        '{"call_id":"call_1","ok":true,"summary":"5 条结果","type":"tool_result"}',
      );
      expect(e.kind, ChatEventKind.toolResult);
      expect(e.toolOk, isTrue);
      expect(e.toolSummary, '5 条结果');
      expect(e.errorCode, isNull);
    });

    test('tool_result 超时失败：带 code=TIMEOUT 与可读 summary', () {
      final e = ChatEvent.fromJsonString(
        '{"call_id":"call_3","ok":false,"summary":"工具执行超时（0.15s），已放弃本次结果","type":"tool_result","code":"TIMEOUT"}',
      );
      expect(e.toolOk, isFalse);
      expect(e.errorCode, 'TIMEOUT');
      expect(e.toolSummary, contains('超时'));
    });

    test('tool_result 取消：code=CANCELLED', () {
      final e = ChatEvent.fromJsonString(
        '{"call_id":"call_2","ok":false,"summary":"已取消","type":"tool_result","code":"CANCELLED"}',
      );
      expect(e.errorCode, 'CANCELLED');
    });
  });

  group('六个失败码的中文文案', () {
    test('全部映射为中文，且不直接回显英文码', () {
      const Map<String, String> expected = <String, String>{
        'TIMEOUT': '超时',
        'BAD_ARGS': '参数',
        'NOT_ALLOWED': '未获授权',
        'CANCELLED': '取消',
        'ROUND_LIMIT': '次数已达上限',
        'TOOL_ERROR': '出错',
      };
      for (final MapEntry<String, String> entry in expected.entries) {
        final String text = toolFailureText(entry.key);
        expect(text, contains(entry.value), reason: '${entry.key} 的文案是「$text」');
        expect(text.contains(entry.key), isFalse, reason: '不能把英文码显示给用户');
      }
    });

    test('未知码回退到 summary，不丢信息', () {
      expect(toolFailureText('SOMETHING_NEW', fallback: '新的失败原因'), '新的失败原因');
    });
  });

  group('取消 vs 失败 的 UI 语义', () {
    Future<void> pump(WidgetTester tester, Widget card) async {
      await tester.pumpWidget(MaterialApp(home: Scaffold(body: Center(child: card))));
      await tester.pumpAndSettle();
    }

    testWidgets('取消显示「已取消」+ 中性色，不显示「失败」', (WidgetTester tester) async {
      await pump(
        tester,
        const ToolCallCard(
          toolName: 'echo',
          status: ToolCallStatus.cancelled,
          errorText: '已取消（你点了停止）',
        ),
      );
      expect(find.text('已取消'), findsOneWidget);
      expect(find.text('失败'), findsNothing);
      expect(find.text('已取消（你点了停止）'), findsOneWidget);
    });

    testWidgets('失败显示「失败」+ 可读原因', (WidgetTester tester) async {
      await pump(
        tester,
        const ToolCallCard(
          toolName: 'echo',
          status: ToolCallStatus.failed,
          errorText: '工具执行超时，已放弃本次结果',
        ),
      );
      expect(find.text('失败'), findsOneWidget);
      expect(find.text('工具执行超时，已放弃本次结果'), findsOneWidget);
    });
  });
}
