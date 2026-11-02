// BYOK 凭据的 Dart 侧测试（fake backend，不需要真 DLL）。
//
// 覆盖任务验收：
// - set → get（掩码正确、无明文）→ delete → get（set:false）；
// - 非法 key / 空值 / 超长 → BAD_INPUT 的可读文案；
// - 不可用（降级）路径的界面文案；
// - 反泄漏断言：UI 与状态里只出现掩码，不出现完整 Key。

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/src/chat/chat_page.dart';
import 'package:yunong/src/core/chat_service.dart';
import 'package:yunong/src/core/secret_service.dart';

/// 内存版假后端：行为与 Rust `MemoryKeyStore` 对齐。
class FakeSecretBackend implements SecretBackend {
  FakeSecretBackend({this.degraded = false});

  final bool degraded;
  final Map<String, String> entries = <String, String>{};

  String? _mask(String value) {
    if (value.length <= 8) {
      return '****';
    }
    return '${value.substring(0, 3)}****${value.substring(value.length - 4)}';
  }

  @override
  Map<String, dynamic> setSecret({
    required String key,
    required String value,
    required String dbPath,
  }) {
    if (!RegExp(r'^[a-z0-9_-]{1,64}$').hasMatch(key)) {
      return <String, dynamic>{'ok': false, 'code': 'BAD_INPUT', 'message': 'key 不合法'};
    }
    if (value.trim().isEmpty) {
      return <String, dynamic>{'ok': false, 'code': 'BAD_INPUT', 'message': '值不能为空'};
    }
    if (value.length > 4096) {
      return <String, dynamic>{'ok': false, 'code': 'BAD_INPUT', 'message': '值超长'};
    }
    entries[key] = value;
    return <String, dynamic>{
      'ok': true,
      'data': <String, dynamic>{
        'set': true,
        'masked': _mask(value),
        'degraded': degraded,
        if (degraded) 'reason': '本次仅内存生效，重启需重新填写',
      },
    };
  }

  @override
  Map<String, dynamic> getSecret({required String key, required String dbPath}) {
    if (!RegExp(r'^[a-z0-9_-]{1,64}$').hasMatch(key)) {
      return <String, dynamic>{'ok': false, 'code': 'BAD_INPUT', 'message': 'key 不合法'};
    }
    final value = entries[key];
    return <String, dynamic>{
      'ok': true,
      'data': <String, dynamic>{
        'set': value != null,
        'masked': value == null ? '' : _mask(value),
        'degraded': degraded,
        if (degraded) 'reason': '本次仅内存生效，重启需重新填写',
      },
    };
  }

  @override
  Map<String, dynamic> deleteSecret({required String key, required String dbPath}) {
    if (!RegExp(r'^[a-z0-9_-]{1,64}$').hasMatch(key)) {
      return <String, dynamic>{'ok': false, 'code': 'BAD_INPUT', 'message': 'key 不合法'};
    }
    return <String, dynamic>{
      'ok': true,
      'data': <String, dynamic>{'deleted': entries.remove(key) != null},
    };
  }
}

const String _plain = 'sk-test-0123456789abcdef0123456789abcdef';

void main() {
  group('SecretService（fake backend）', () {
    test('set → get 掩码 → delete → get(set:false)', () async {
      final backend = FakeSecretBackend();
      final service = SecretService(backend: backend, dbPath: '/tmp/x.db');

      final saved = await service.save(SecretService.byokKeyName, _plain);
      expect(saved.set, isTrue);
      expect(saved.masked, 'sk-****cdef');
      expect(saved.masked.contains('0123456789abcdef'), isFalse);

      final status = await service.status(SecretService.byokKeyName);
      expect(status.set, isTrue);
      expect(status.masked, 'sk-****cdef');

      expect(await service.delete(SecretService.byokKeyName), isTrue);
      final after = await service.status(SecretService.byokKeyName);
      expect(after.set, isFalse);
      expect(after.display, '未设置');
      // 幂等
      expect(await service.delete(SecretService.byokKeyName), isFalse);
    });

    test('非法 key / 空值 / 超长 → BAD_INPUT + 可读文案', () async {
      final service = SecretService(backend: FakeSecretBackend(), dbPath: '/tmp/x.db');
      for (final Future<SecretStatus> Function() call in <Future<SecretStatus> Function()>[
        () => service.save('BAD KEY', _plain),
        () => service.save(SecretService.byokKeyName, ''),
        () => service.save(SecretService.byokKeyName, 'a' * 4097),
      ]) {
        try {
          await call();
          fail('应当抛出 SecretException');
        } on SecretException catch (failure) {
          expect(failure.code, 'BAD_INPUT');
          expect(failure.userHint, isNotEmpty);
          expect(failure.userHint.contains('sk-'), isFalse);
        }
      }
      // get/delete 的非法 key 同样报错
      await expectLater(
        service.status('BAD KEY'),
        throwsA(isA<SecretException>().having((SecretException e) => e.code, 'code', 'BAD_INPUT')),
      );
    });

    test('降级：degraded + 明确“仅内存生效”文案', () async {
      final service = SecretService(backend: FakeSecretBackend(degraded: true), dbPath: '/tmp/x.db');
      final saved = await service.save(SecretService.byokKeyName, _plain);
      expect(saved.degraded, isTrue);
      expect(saved.degradedHint, contains('仅内存生效'));
    });
  });

  group('UI：设置页只显示掩码', () {
    testWidgets('顶栏显示 sk-****cdef，降级时提示仅内存生效', (WidgetTester tester) async {
      final backend = FakeSecretBackend(degraded: true);
      backend.entries[SecretService.byokKeyName] = _plain;
      final secrets = SecretService(backend: backend, dbPath: '/tmp/x.db');
      final chats = ChatService(backend: _NoopChatBackend());

      await tester.pumpWidget(MaterialApp(
        home: Scaffold(body: ChatPage(service: chats, secretService: secrets)),
      ));
      await tester.pumpAndSettle();

      expect(find.textContaining('sk-****cdef'), findsOneWidget);
      expect(find.text(_plain), findsNothing, reason: 'UI 不得出现明文');
      expect(find.textContaining('仅内存生效'), findsOneWidget);
    });

    testWidgets('未设置时显示“未设置”，且没有明文', (WidgetTester tester) async {
      final secrets = SecretService(backend: FakeSecretBackend(), dbPath: '/tmp/x.db');
      final chats = ChatService(backend: _NoopChatBackend());
      await tester.pumpWidget(MaterialApp(
        home: Scaffold(body: ChatPage(service: chats, secretService: secrets)),
      ));
      await tester.pumpAndSettle();
      expect(find.textContaining('未设置'), findsOneWidget);
      expect(find.text(_plain), findsNothing);
    });
  });
}

/// 不会发请求的 chat 后端（本测试只关心 Key 展示）。
class _NoopChatBackend implements ChatBackend {
  @override
  int start(String requestJson, bool Function(String chunkJson) onChunk) => 1;

  @override
  void cancel(int handle) {}
}
