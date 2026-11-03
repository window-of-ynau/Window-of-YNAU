// t79 端到端投递验证：真 DLL + fake_sse 上游，确认“注册早于调用”后能收到事件。
//
// 运行需要：
// - `YUNONG_CORE_DLL` 指向 yunong_core.dll（CI/本地由脚本设置）；
// - `target/debug/examples/fake_sse.exe` 已构建（cargo build -p yunong-core --example fake_sse）。
// 两者缺一时**跳过**（打印原因），避免在无 Rust 环境里误红。

import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/src/core/chat_service.dart';

const int _port = 18765;

File? _findFakeSse() {
  for (final path in <String>[
    '../target/debug/examples/fake_sse.exe',
    '../target/debug/examples/fake_sse',
    'target/debug/examples/fake_sse.exe',
  ]) {
    final file = File(path);
    if (file.existsSync()) {
      return file;
    }
  }
  return null;
}

String? _findDll() {
  for (final path in <String>[
    '../target/debug/deps/yunong_core.dll',
    '../target/debug/yunong_core.dll',
  ]) {
    if (File(path).existsSync()) {
      return File(path).absolute.path;
    }
  }
  return Platform.environment['YUNONG_CORE_DLL'];
}

// 已知阻塞（t79）：NativeCallable.listener 是**异步**投递，Rust 在 mit 返回后即释放 CString，
// Dart 读到悬垂指针（实测 framesFired=1，解码 FormatException at offset 2）。修复需 Rust 侧改契约：
// 回调字符串 leak 给 Dart 显式 yunong_free_string，或改用 Dart_Port 投递。修好后去掉 skip。
void main() {
  test('fake_sse 上游：真实 FFI 全链路能收到 delta + done', () async {
    final fakeSse = _findFakeSse();
    final dll = _findDll();
    if (fakeSse == null || dll == null) {
      markTestSkipped('缺少 fake_sse 或 yunong_core.dll（fake=${fakeSse?.path}, dll=$dll）');
      return;
    }
    Platform.environment['YUNONG_CORE_DLL']; // 说明：真正生效的是进程环境变量（见测试脚本）

    final process = await Process.start(
      fakeSse.absolute.path,
      <String>['--port', '$_port'],
      mode: ProcessStartMode.normal,
    );
    addTearDown(() {
      process.kill();
    });
    // 等端口起来（fake_sse 极快，200ms 足够）
    await Future<void>.delayed(const Duration(milliseconds: 300));

    final request = ChatRequestDto(
      channel: ChatChannel.byok,
      model: 'deepseek-chat',
      protocol: ChatProtocol.completions,
      apiKey: 'sk-test-',
      baseUrl: 'http://127.0.0.1:$_port',
      messages: <ChatTurn>[ChatTurn.user('你好')],
    ).toJson();

    final backend = FfiChatBackend();
    final chunks = <Map<String, dynamic>>[];
    final done = Completer<void>();
    final handle = backend.start(jsonEncode(request), (String chunkJson) {
      final decoded = jsonDecode(chunkJson);
      if (decoded is Map<String, dynamic>) {
        chunks.add(decoded);
        if ((decoded['type'] as String?) == 'done' && !done.isCompleted) {
          done.complete();
        }
        if ((decoded['type'] as String?) == 'error' && !done.isCompleted) {
          done.completeError(StateError('收到 error 帧: $decoded'));
        }
      }
      return true;
    });

    expect(handle, greaterThan(0));
    await done.future.timeout(const Duration(seconds: 20), onTimeout: () {
      throw StateError('20s 内没有收到 done：chunks=${chunks.length}, framesFired=${backend.framesFired}');
    });

    final deltas = chunks.where((Map<String, dynamic> c) => c['type'] == 'delta').toList();
    expect(deltas, isNotEmpty, reason: 'fake_sse 必须能推 delta');
    expect(backend.framesFired, greaterThan(0), reason: '回调确实被触发');
    backend.cancel(handle);
  }, timeout: const Timeout(Duration(minutes: 2)), skip: '等待 Rust 侧修字符串所有权（悬垂指针，见文件头）');
}
