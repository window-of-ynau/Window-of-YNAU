// t106：真 DLL 端到端 —— 工具事件顺序、call_id 配对、终态、失败码。
//
// 依赖：
// - `YUNONG_CORE_DLL` 指向 debug 产物（本地：../target/debug/deps/yunong_core.dll）；
// - `target/debug/examples/fake_sse.exe` 已构建（cargo build -p yunong-core --example fake_sse）。
// 缺任一条件时**跳过**并打印原因；本地已真跑（见任务报告）。
//
// wire 形状（Rust 实测）：
//   {"args":{...},"call_id":"call_1","index":0,"name":"memory_search","type":"tool_call"}
//   {"call_id":"call_1","ok":true,"summary":"找到 1 条","type":"tool_result"}
//   {"type":"delta",...} → {"type":"done","finish":"stop"}

import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/src/chat/tool_call_card.dart';
import 'package:yunong/src/core/chat_service.dart';
import 'package:yunong/src/core/memory_service.dart';

final List<File> _spawned = <File>[];

File? _findFakeSse() {
  for (final path in <String>[
    '../target/debug/examples/fake_sse.exe',
    '../target/debug/examples/fake_sse',
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

/// 跑一轮真 DLL 流，返回收到的原始 chunk 文本。
Future<List<String>> _runOnce({
  required int port,
  required String mode,
  required String dbDir,
}) async {
  final process = await Process.start(
    _findFakeSse()!.absolute.path,
    <String>['--port', '$port', '--mode', mode],
  );
  _spawned.add(File('${process.pid}'));
  addTearDown(process.kill);
  final List<String> serverLog = <String>[];
  process.stdout.transform(utf8.decoder).transform(const LineSplitter()).listen(serverLog.add);
  process.stderr.transform(utf8.decoder).transform(const LineSplitter()).listen(serverLog.add);
  await Future<void>.delayed(const Duration(milliseconds: 400));

  final request = ChatRequestDto(
    channel: ChatChannel.byok,
    model: 'deepseek-chat',
    protocol: ChatProtocol.completions,
    apiKey: 'sk-test-',
    baseUrl: 'http://127.0.0.1:$port',
    dbPath: dbDir,
    messages: <ChatTurn>[ChatTurn.user('帮我查一下课表')],
  ).toJson();

  final backend = FfiChatBackend();
  final List<String> raw = <String>[];
  final done = Completer<void>();
  final handle = backend.start(request, (String chunkJson) {
    raw.add(chunkJson);
    if ((jsonTypeOf(chunkJson) == 'done' || jsonTypeOf(chunkJson) == 'error') && !done.isCompleted) {
      done.complete();
    }
    return true;
  });
  expect(handle, greaterThan(0));
  // t110：假上游轮次漂移已修（按请求体判定），这里改回**硬断言**：done 必须存在。
  try {
    await done.future.timeout(const Duration(seconds: 30));
  } on TimeoutException {
    // t110：把假上游 stdout 贴出来，区分“第二轮没发出”还是“第二轮响应没解析完”
    // ignore: avoid_print
    print('T110_FAKE_SSE_LOG:\n${serverLog.join('\n')}');
    rethrow;
  }
  backend.cancel(handle);
  return raw;
}

void main() {
  final File? fakeSse = _findFakeSse();
  final String? dll = _findDll();
  final String envDll = Platform.environment['YUNONG_CORE_DLL'] ?? '';

  test('真 DLL + fake_sse --mode tool：tool_call → tool_result → delta → done，配对且终态', () async {
    if (fakeSse == null || dll == null || envDll.isEmpty) {
      markTestSkipped('需要 YUNONG_CORE_DLL 与 fake_sse 产物：fake=${fakeSse?.path}, dll=$dll, env=$envDll');
      return;
    }
    final Directory dbDir = Directory.systemTemp.createTempSync('t106-db');
    addTearDown(() => dbDir.deleteSync(recursive: true));

    // 实测：Rust 把 db_path 当**数据库文件**打开（传目录会报 unable to open database file）
    // 实测：Rust 把 db_path 当数据库文件打开（传目录会报 unable to open database file）
    final String dbFile = "${dbDir.path}/yunong.db";

    // t107：先种一条含「雨伞」的记忆，覆盖"搜到内容"的成功路径（否则只能测到空结果）
    final MemoryService memory = MemoryService(dbPath: dbFile);
    await memory.save(kind: MemoryKind.note, title: '雨伞', content: '雨伞是黑色的');

    final List<String> raw =
        await _runOnce(port: 18801, mode: 'tool', dbDir: dbFile);
    // 原始事件（供报告引用）
    // ignore: avoid_print
    print('T106_RAW_TOOL_EVENTS=${raw.join('\n')}');

    final List<ChatEvent> events = raw.map(ChatEvent.fromJsonString).toList();
    final List<ChatEvent> calls = events.where((ChatEvent e) => e.kind == ChatEventKind.toolCall).toList();
    final List<ChatEvent> results =
        events.where((ChatEvent e) => e.kind == ChatEventKind.toolResult).toList();

    expect(calls, isNotEmpty, reason: '必须收到 tool_call（真工具事件，不是骨架）');
    expect(results, isNotEmpty, reason: '必须收到 tool_result');

    // 顺序：每个 tool_call 的下标 < 同 call_id 的 tool_result 下标
    for (final ChatEvent call in calls) {
      final int callIndex = events.indexOf(call);
      final int resultIndex = events.indexWhere(
        (ChatEvent e) => e.kind == ChatEventKind.toolResult && e.toolCallId == call.toolCallId,
      );
      expect(resultIndex, greaterThan(callIndex),
          reason: '${call.toolCallId} 的 tool_result 必须在 tool_call 之后');
    }

    // 配对 + 终态：每个 call 都有 result；成功时 ok=true
    for (final ChatEvent call in calls) {
      final ChatEvent? result = results
          .where((ChatEvent e) => e.toolCallId == call.toolCallId)
          .cast<ChatEvent?>()
          .firstWhere((ChatEvent? _) => true, orElse: () => null);
      expect(result, isNotNull, reason: '${call.toolCallId} 没有对应 tool_result（卡片会停在“进行中”）');
      expect(result!.toolOk, isTrue, reason: 'tool 模式应成功');
      // t107：种了记忆后必须走「搜到内容」路径（不能只证明调用成功）
      expect(result.toolSummary, isNotNull);
      expect(result.toolSummary!, contains('找到'), reason: '实际 summary=' + result.toolSummary!);
      expect(result.toolSummary!, isNot(contains('没搜到')));

      // t110：帧数量级 + done 硬断言（有结果路径约 14 帧）
      final int deltaCount = events.where((ChatEvent e) => e.kind == ChatEventKind.delta).length;
      expect(events.length, greaterThanOrEqualTo(10), reason: '总帧数过少，可能第二轮没跑');
      expect(deltaCount, greaterThanOrEqualTo(5), reason: 'delta 过少，可能响应没解析完');
      expect(events.any((ChatEvent e) => e.kind == ChatEventKind.done), isTrue, reason: '必须有 done');
    }

    // 流尾 done：当前 Rust 在“搜到内容”路径上会停住不发 done（已在上面 T106_WARN 打印）。
    // 这里不把它当作本测试的失败条件——但会记录，避免问题被掩盖。
    if (!events.any((ChatEvent e) => e.kind == ChatEventKind.done)) {
      // ignore: avoid_print
      print('T106_WARN: 流未以 done 结束（已知 Rust 侧问题）');
    }
  }, timeout: const Timeout(Duration(minutes: 2)));

  test('真 DLL + fake_sse --mode tool-fail：ok=false、code=BAD_ARGS、中文文案', () async {
    if (fakeSse == null || dll == null || envDll.isEmpty) {
      markTestSkipped('需要 YUNONG_CORE_DLL 与 fake_sse 产物');
      return;
    }
    final Directory dbDir = Directory.systemTemp.createTempSync('t106-db-fail');
    addTearDown(() => dbDir.deleteSync(recursive: true));

    final List<String> raw = await _runOnce(port: 18802, mode: 'tool-fail', dbDir: '${dbDir.path}/yunong.db');
    // ignore: avoid_print
    print('T106_RAW_TOOLFAIL_EVENTS=${raw.join('\n')}');

    final List<ChatEvent> events = raw.map(ChatEvent.fromJsonString).toList();
    final ChatEvent result = events.firstWhere(
      (ChatEvent e) => e.kind == ChatEventKind.toolResult,
      orElse: () => throw StateError('tool-fail 模式没有 tool_result：$raw'),
    );
    expect(result.toolOk, isFalse);
    expect(result.errorCode, 'BAD_ARGS');
    // 中文文案（不把英文码显示给用户）
    final String text = toolFailureText(result.errorCode, fallback: result.toolSummary);
    expect(text, contains('参数'));
    expect(text.contains('BAD_ARGS'), isFalse);
  }, timeout: const Timeout(Duration(minutes: 2)));

  test('空结果路径（不种记忆）也要有 done', () async {
    if (fakeSse == null || dll == null || envDll.isEmpty) {
      markTestSkipped('需要 YUNONG_CORE_DLL 与 fake_sse 产物');
      return;
    }
    final Directory dbDir = Directory.systemTemp.createTempSync('t110-empty');
    addTearDown(() => dbDir.deleteSync(recursive: true));

    final List<String> raw = await _runOnce(
      port: 18821,
      mode: 'tool',
      dbDir: '${dbDir.path}/yunong.db',
    );
    final List<ChatEvent> events = raw.map(ChatEvent.fromJsonString).toList();
    final ChatEvent result = events.firstWhere((ChatEvent e) => e.kind == ChatEventKind.toolResult);
    expect(result.toolOk, isTrue);
    expect(result.toolSummary, contains('没搜到'), reason: '空库应走“没搜到”路径');
    expect(events.any((ChatEvent e) => e.kind == ChatEventKind.done), isTrue,
        reason: '空结果路径也必须有 done');
  }, timeout: const Timeout(Duration(minutes: 2)));
}

/// 只取 JSON 的 `type` 字段（避免把整段 JSON 塞进判断）。
String? jsonTypeOf(String chunk) {
  final RegExpMatch? m = RegExp(r'"type"\s*:\s*"([a-z_]+)"').firstMatch(chunk);
  return m?.group(1);
}
