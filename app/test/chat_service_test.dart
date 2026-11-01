// 流式对话的 Dart 侧测试（fake backend，不需要真 DLL / 真网络）。
//
// 覆盖：
// - 逐字 delta 累加、usage 落地、done 终止；
// - error 事件 → 可读文案（不显示原始串）；
// - cancel 会调用后端 cancel，且后续事件不再渲染；
// - ChatRequestDto 的 JSON 形状（含通道/协议/Key 字段）。

import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/src/core/chat_service.dart';

/// 可编排的假后端：按脚本逐条回放 chunk。
class ScriptedBackend implements ChatBackend {
  ScriptedBackend(this.script);

  final List<String> script;
  final List<String> startedRequests = <String>[];
  final List<int> cancelled = <int>[];
  bool Function(String chunkJson)? _onChunk;
  int _nextHandle = 1;

  @override
  int start(String requestJson, bool Function(String chunkJson) onChunk) {
    startedRequests.add(requestJson);
    _onChunk = onChunk;
    final handle = _nextHandle++;
    // 同步回放（模拟"立刻有 chunk"）
    for (final chunk in script) {
      if (!onChunk(chunk)) {
        break;
      }
    }
    return handle;
  }

  /// 测试里手动推一条（模拟流式后续到达）。
  void push(String chunkJson) => _onChunk?.call(chunkJson);

  @override
  void cancel(int handle) => cancelled.add(handle);
}

ChatRequestDto request({String? key = 'sk-test'}) => ChatRequestDto(
      channel: ChatChannel.byok,
      model: 'deepseek-chat',
      apiKey: key,
      messages: <ChatTurn>[ChatTurn.user('你好')],
    );

void main() {
  group('事件解析', () {
    test('delta / usage / done', () {
      final delta = ChatEvent.fromJsonString('{"type":"delta","text":"你"}');
      expect(delta.kind, ChatEventKind.delta);
      expect(delta.text, '你');

      final usage = ChatEvent.fromJsonString(
        '{"type":"usage","usage":{"prompt_tokens":12,"completion_tokens":34}}',
      );
      expect(usage.kind, ChatEventKind.usage);
      expect(usage.promptTokens, 12);
      expect(usage.completionTokens, 34);

      final done = ChatEvent.fromJsonString('{"type":"done","finish":"stop"}');
      expect(done.kind, ChatEventKind.done);
      expect(done.isTerminal, isTrue);
    });

    test('未知类型 / 坏 JSON 不崩', () {
      expect(ChatEvent.fromJsonString('{"type":"mystery"}').kind, ChatEventKind.unknown);
      expect(ChatEvent.fromJsonString('not json').kind, ChatEventKind.unknown);
    });

    test('错误码 → 可读文案（不含原始串）', () {
      for (final code in <String>[
        'UNAUTHORIZED',
        'QUOTA_EXCEEDED',
        'RATE_LIMITED',
        'CONCURRENCY_LIMIT',
        'UPSTREAM_TIMEOUT',
        'WHATEVER',
      ]) {
        final text = describeChatError(code);
        expect(text, isNotEmpty);
        expect(text, isNot(contains('Exception')));
        expect(text, isNot(contains(code)), reason: '不该把错误码原样丢给用户：$code');
      }
      expect(describeChatError('UNAUTHORIZED'), contains('API Key'));
      expect(describeChatError('WHATEVER'), contains('重试'));
    });
  });

  group('ChatService', () {
    test('逐字累加 + 用量 + done 结束', () async {
      final backend = ScriptedBackend(<String>[
        '{"type":"meta","request_id":"r1","model":"deepseek-chat"}',
        '{"type":"delta","text":"你"}',
        '{"type":"delta","text":"好"}',
        '{"type":"usage","usage":{"prompt_tokens":5,"completion_tokens":2}}',
        '{"type":"done","finish":"stop"}',
      ]);
      final service = ChatService(backend: backend);
      final events = await service.send(request()).events.toList();

      expect(events.map((ChatEvent e) => e.kind), containsAll(<ChatEventKind>[
        ChatEventKind.meta,
        ChatEventKind.delta,
        ChatEventKind.usage,
        ChatEventKind.done,
      ]));
      final text = events
          .where((ChatEvent e) => e.kind == ChatEventKind.delta)
          .map((ChatEvent e) => e.text)
          .join();
      expect(text, '你好');
      expect(events.last.kind, ChatEventKind.done);
    });

    test('error 事件 → 可读文案', () async {
      final backend = ScriptedBackend(<String>[
        '{"type":"error","code":"UNAUTHORIZED","message":"no key"}',
      ]);
      final events = await ChatService(backend: backend).send(request(key: '')).events.toList();
      expect(events.single.kind, ChatEventKind.error);
      expect(describeChatError(events.single.errorCode), contains('API Key'));
    });

    test('cancel 调用后端 cancel（停止消费）', () async {
      final backend = ScriptedBackend(<String>[]);
      final service = ChatService(backend: backend);
      final stream = service.send(request());
      expect(stream.events, isNotNull);
      stream.cancel();
      expect(backend.cancelled, isNotEmpty);
    });

    test('请求 JSON 形状：通道/协议/Key/messages', () {
      final json = ChatRequestDto(
        channel: ChatChannel.cloud,
        protocol: ChatProtocol.anthropic,
        model: 'claude-x',
        cloudToken: 'tok',
        messages: <ChatTurn>[ChatTurn.user('hi'), ChatTurn.assistant('yo')],
      ).toJson();
      expect(json, contains('"channel":"cloud"'));
      expect(json, contains('"protocol":"anthropic_messages"'));
      expect(json, contains('"cloud_token":"tok"'));
      expect(json, isNot(contains('api_key')));
      expect(json, contains('"role":"user"'));
      expect(json, contains('"role":"assistant"'));
    });

    test('BYOK 请求带 api_key，且空串时不带字段', () {
      expect(request(key: 'sk-1').toJson(), contains('"api_key":"sk-1"'));
      expect(request(key: '').toJson(), isNot(contains('api_key')));
    });
  });
}
