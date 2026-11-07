// t94 回归：
// 1) 桥接脚本必须“先排队、后补发”（旧实现 polyfill 未就绪时静默丢消息）；
// 2) Anthropic + DeepSeek 这类上游要在发送前拦住，而不是笼统 UPSTREAM_ERROR。

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/src/campus/portal_api.dart';
import 'package:yunong/src/campus/webview_session.dart';
import 'package:yunong/src/chat/chat_page.dart';
import 'package:yunong/src/chat/chat_settings_section.dart';
import 'package:yunong/src/core/chat_service.dart';
import 'package:yunong/src/core/secret_service.dart';

class _NoopChatBackend implements ChatBackend {
  @override
  int start(String requestJson, bool Function(String chunkJson) onChunk) => 1;

  @override
  void cancel(int handle) {}
}

class _SetSecrets implements SecretBackend {
  @override
  Map<String, dynamic> setSecret({required String key, required String value, required String dbPath}) =>
      <String, dynamic>{'ok': true, 'data': <String, dynamic>{'set': true, 'masked': 'sk-****cdef'}};

  @override
  Map<String, dynamic> getSecret({required String key, required String dbPath}) => <String, dynamic>{
        'ok': true,
        'data': <String, dynamic>{'set': true, 'masked': 'sk-****cdef', 'degraded': false, 'broken': false},
      };

  @override
  Map<String, dynamic> deleteSecret({required String key, required String dbPath}) =>
      <String, dynamic>{'ok': true, 'data': <String, dynamic>{'deleted': true}};
}

Future<ChatPageState> _pumpChat(WidgetTester tester, ChatSettingsController shared) async {
  await tester.pumpWidget(MaterialApp(
    home: Scaffold(
      body: ChatPage(
        service: ChatService(backend: _NoopChatBackend()),
        secretService: SecretService(backend: _SetSecrets(), dbPath: '/tmp/t94.db'),
        chatSettings: shared,
        dbPath: '/tmp/t94.db',
      ),
    ),
  ));
  await tester.pumpAndSettle();
  return tester.state<ChatPageState>(find.byType(ChatPage));
}

void main() {
  group('P0-1 桥接：polyfill 未就绪时不能丢消息', () {
    for (final entry in <String, String>{
      'portal_api（门户课表脚本）': kPortalScheduleScript,
      'webview_session（探测脚本）': kCampusProbeScript,
    }.entries) {
      test('${entry.key} 必须排队 + 定时补发', () {
        final String script = entry.value;
        expect(script.contains('PENDING'), isTrue, reason: '需要一个待发队列');
        expect(
          script.contains('setInterval('),
          isTrue,
          reason: '需要在 polyfill 就绪后定时补发（否则消息永久丢失）',
        );
        // 旧实现在 polyfill 缺失时直接 return false（静默丢弃）——这条断言防止回退
        expect(
          script.contains('if (!window.flutter_inappwebview) { return false; }'),
          isFalse,
          reason: '不得回到“polyfill 缺失就静默丢弃”的旧实现',
        );
      });
    }

    test('诊断里能看到排队数量（bridgeQueued）', () {
      expect(kPortalScheduleScript.contains('bridgeQueued'), isTrue);
    });
  });

  group('P1-3 Anthropic + 只支持 OpenAI 的上游', () {
    test('isOpenAiOnlyUpstream 识别 DeepSeek/OpenAI', () {
      expect(isOpenAiOnlyUpstream('https://api.deepseek.com'), isTrue);
      expect(isOpenAiOnlyUpstream('https://api.openai.com/v1'), isTrue);
      expect(isOpenAiOnlyUpstream('https://api.anthropic.com'), isFalse);
    });

    testWidgets('发送前拦下：提示“不支持 Anthropic Messages”而不是 UPSTREAM_ERROR', (WidgetTester tester) async {
      final shared = ChatSettingsController();
      shared.settings.protocol = 'anthropic_messages';
      shared.settings.baseUrl = 'https://api.deepseek.com';

      final state = await _pumpChat(tester, shared);

      // 直接断言通道就绪判定的语义（不走界面文案，避免读错控件）
      final ({bool ready, String reason, String code}) channel = state.channelStateForTest;
      expect(channel.ready, isFalse);
      expect(channel.code, 'PROTOCOL_UNSUPPORTED');
      expect(channel.reason, contains('不支持 Anthropic Messages'));
    });

    testWidgets('换成 openai_completions 后即可发送（不再误拦）', (WidgetTester tester) async {
      final shared = ChatSettingsController();
      shared.settings.protocol = 'openai_completions';
      shared.settings.baseUrl = 'https://api.deepseek.com';

      final state = await _pumpChat(tester, shared);
      final ({bool ready, String reason, String code}) channel = state.channelStateForTest;
      expect(channel.ready, isTrue);
    });
  });
}
