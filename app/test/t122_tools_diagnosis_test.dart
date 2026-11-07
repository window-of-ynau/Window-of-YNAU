// t122：工具不触发排查——db_path 仍在请求里、诊断显示 tools_enabled、tool_choice 调试开关。

import 'dart:convert';

import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/src/core/chat_service.dart';

Map<String, dynamic> _json(String raw) => jsonDecode(raw) as Map<String, dynamic>;

ChatRequestDto _dto({String dbPath = '/tmp/yn/yunong.db', String? toolChoice, String model = 'deepseek-chat'}) =>
    ChatRequestDto(
      channel: ChatChannel.byok,
      model: model,
      messages: <ChatTurn>[ChatTurn.user('帮我查课表')],
      dbPath: dbPath,
      dataDir: '/tmp/yn',
      toolChoice: toolChoice,
    );

void main() {
  group('一、db_path 仍在请求里（启用内置工具的关键）', () {
    test('db_path 非空且以 .db 结尾', () {
      final Map<String, dynamic> json = _json(_dto().toJson());
      expect(json['db_path'], isNotNull);
      expect('${json['db_path']}'.trim(), isNotEmpty);
      expect(json['db_path'], endsWith('.db'));
    });

    test('data_dir 与 db_path 是两个不同的值（目录 vs 文件）', () {
      final Map<String, dynamic> json = _json(_dto().toJson());
      expect(json['data_dir'], '/tmp/yn');
      expect(json['db_path'], '/tmp/yn/yunong.db');
      expect(json['data_dir'], isNot(json['db_path']));
    });

    test('没有 db_path 时请求里不出现该键（不误开工具）', () {
      final ChatRequestDto dto = ChatRequestDto(
        channel: ChatChannel.byok,
        model: 'deepseek-chat',
        messages: <ChatTurn>[ChatTurn.user('hi')],
      );
      expect(dto.toJson(), isNot(contains('db_path')));
    });
  });

  group('三、model 与界面同源', () {
    test('请求里的 model 就是传进来的值（不写死）', () {
      expect(_json(_dto(model: 'deepseek-reasoner').toJson())['model'], 'deepseek-reasoner');
      expect(_json(_dto(model: 'deepseek-chat').toJson())['model'], 'deepseek-chat');
    });
  });

  group('二、诊断里的工具状态', () {
    test('tools_enabled=true | tools=[a, b] 形态', () {
      expect(
        toolDiagnosticsText(toolsEnabled: true, tools: <String>['timetable_query', 'memory_search']),
        'tools_enabled=true | tools=[timetable_query, memory_search]',
      );
    });

    test('没挂工具时一眼可见', () {
      expect(toolDiagnosticsText(toolsEnabled: false, tools: <String>[]),
          'tools_enabled=false | tools=[]');
    });

    test('meta 未回报时显示未知（不假装有/没有）', () {
      expect(toolDiagnosticsText(), 'tools_enabled=未知');
    });

    test('meta 帧解析 tools_enabled / tools', () {
      final ChatEvent event = ChatEvent.fromJsonString(
        '{"type":"meta","request_id":"r1","model":"deepseek-chat","channel":"byok",'
        '"tools_enabled":true,"tools":["timetable_query","memory_search"]}',
      );
      expect(event.toolsEnabled, isTrue);
      expect(event.tools, <String>['timetable_query', 'memory_search']);
    });
  });

  group('调试开关 tool_choice', () {
    test('默认不带 tool_choice', () {
      expect(_dto().toJson(), isNot(contains('tool_choice')));
    });

    test('打开后带 tool_choice=required', () {
      final Map<String, dynamic> json = _json(_dto(toolChoice: 'required').toJson());
      expect(json['tool_choice'], 'required');
    });
  });
}
