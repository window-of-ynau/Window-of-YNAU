// 流式对话的 Dart 侧：FFI 回调桥接 + 事件模型 + 错误文案。
//
// # 形状（前端先定形状，Rust 侧配合）
//
// ```dart
// final chat = ChatService();          // 内部复用端侧核心 DLL
// final stream = chat.send(ChatRequestDto(
//   channel: ChatChannel.byok,          // ★ 通道可选
//   model: 'deepseek-chat',
//   apiKey: key,                        // ★ 只进 DLL，不进日志
//   messages: <ChatTurn>[ChatTurn.user('你好')],
// ));
// stream.events.listen((ChatEvent event) { ... });  // 逐字 delta / 用量 / 结束 / 错误
// stream.cancel();                       // 停止生成（不再消费上游）
// ```
//
// 回调用 `NativeCallable.listener`：Rust 在后台线程回调，Dart 侧通过端口收，
// 不阻塞 UI isolate。字符串由 Rust 在回调期间持有、回调返回即释放，Dart 不 free。

import 'dart:async';
import 'dart:convert';
import 'dart:ffi';

import 'package:flutter/foundation.dart';

import 'package:ffi/ffi.dart';

import 'ffi_bindings.dart';

/// 通道：BYOK 直连厂商 / 套餐代理。
enum ChatChannel {
  byok('byok', 'BYOK 直连'),
  cloud('cloud', '套餐通道');

  const ChatChannel(this.wire, this.label);

  /// 传给 Rust 的字符串（**不要改**：Rust 侧按这两个值分支）
  final String wire;
  final String label;
}

/// 协议（三协议之一）。
enum ChatProtocol {
  completions('openai_completions', 'OpenAI Completions'),
  responses('openai_responses', 'OpenAI Responses'),
  anthropic('anthropic_messages', 'Anthropic Messages');

  const ChatProtocol(this.wire, this.label);

  final String wire;
  final String label;
}

/// 一条对话消息（发请求用）。
class ChatTurn {
  const ChatTurn({required this.role, required this.content});

  factory ChatTurn.user(String content) => ChatTurn(role: 'user', content: content);
  factory ChatTurn.assistant(String content) =>
      ChatTurn(role: 'assistant', content: content);

  final String role;
  final String content;

  Map<String, dynamic> toJson() => <String, dynamic>{'role': role, 'content': content};
}

/// 一次流式请求。
class ChatRequestDto {
  const ChatRequestDto({
    required this.channel,
    required this.model,
    required this.messages,
    this.protocol = ChatProtocol.completions,
    this.apiKey,
    this.secretKey,
    this.dataDir,
    this.cloudToken,
    this.baseUrl,
    this.cloudBaseUrl,
    this.maxOutputTokens,
    this.sessionId,
    this.turnId,
  });

  final ChatChannel channel;
  final String model;
  final List<ChatTurn> messages;
  final ChatProtocol protocol;

  /// BYOK 的 Key（**仅命令行/测试**用；App 走 [secretKey]，明文不经过 Dart）
  final String? apiKey;

  /// 安全存储里的 key 名（如 `byok_api_key`）：由 Rust 自己取明文
  final String? secretKey;

  /// 应用数据目录（Rust 用它定位 secret store）
  final String? dataDir;

  /// 套餐通道的访问令牌
  final String? cloudToken;
  final String? baseUrl;
  final String? cloudBaseUrl;
  final int? maxOutputTokens;
  final String? sessionId;
  final String? turnId;

  String toJson() => jsonEncode(<String, dynamic>{
        'channel': channel.wire,
        'protocol': protocol.wire,
        'model': model,
        'messages': messages.map((ChatTurn turn) => turn.toJson()).toList(),
        if (secretKey != null && secretKey!.isNotEmpty) 'secret_key': secretKey,
        if (dataDir != null && dataDir!.isNotEmpty) 'data_dir': dataDir,
        // 仅测试/命令行用；App 正常路径不会带明文
        if (apiKey != null && apiKey!.isNotEmpty) 'api_key': apiKey,
        if (cloudToken != null && cloudToken!.isNotEmpty) 'cloud_token': cloudToken,
        if (baseUrl != null && baseUrl!.isNotEmpty) 'base_url': baseUrl,
        if (cloudBaseUrl != null && cloudBaseUrl!.isNotEmpty) 'cloud_base_url': cloudBaseUrl,
        if (maxOutputTokens != null) 'max_output_tokens': maxOutputTokens,
        if (sessionId != null) 'session_id': sessionId,
        if (turnId != null) 'turn_id': turnId,
      });
}

/// 流式事件种类（与 `Chunk` 的 `type` 一一对应）。
enum ChatEventKind { meta, delta, toolCall, toolResult, usage, done, error, ping, unknown }

/// 一条流式事件。
class ChatEvent {
  const ChatEvent({
    required this.kind,
    this.text,
    this.requestId,
    this.model,
    this.channel,
    this.errorCode,
    this.errorMessage,
    this.promptTokens,
    this.completionTokens,
    this.finishReason,
  });

  final ChatEventKind kind;
  final String? text;
  final String? requestId;
  final String? model;
  final String? channel;
  final String? errorCode;
  final String? errorMessage;
  final int? promptTokens;
  final int? completionTokens;
  final String? finishReason;

  bool get isTerminal => kind == ChatEventKind.done || kind == ChatEventKind.error;

  /// 解析一条 Chunk JSON（容错：未知字段/未知类型不崩）。
  factory ChatEvent.fromJsonString(String raw) {
    Object? decoded;
    try {
      decoded = jsonDecode(raw);
    } on FormatException {
      return const ChatEvent(kind: ChatEventKind.unknown);
    }
    if (decoded is! Map<String, dynamic>) {
      return const ChatEvent(kind: ChatEventKind.unknown);
    }
    final type = decoded['type'] as String?;
    final usage = decoded['usage'];
    return ChatEvent(
      kind: _kindOf(type),
      text: decoded['text'] as String?,
      requestId: decoded['request_id'] as String?,
      model: decoded['model'] as String?,
      channel: (decoded['channel'] as String?) ??
          ((decoded['channel'] as Map<dynamic, dynamic>?)?['channel'] as String?),
      errorCode: (decoded['code'] as String?) ?? _codeOf(decoded['code']),
      errorMessage: decoded['message'] as String?,
      promptTokens: _intOf(decoded['prompt_tokens'] ?? (usage is Map ? usage['prompt_tokens'] : null)),
      completionTokens:
          _intOf(decoded['completion_tokens'] ?? (usage is Map ? usage['completion_tokens'] : null)),
      finishReason: decoded['finish'] as String? ?? decoded['reason'] as String?,
    );
  }

  static ChatEventKind _kindOf(String? type) => switch (type) {
        'meta' => ChatEventKind.meta,
        'delta' => ChatEventKind.delta,
        'tool_call' => ChatEventKind.toolCall,
        'tool_result' => ChatEventKind.toolResult,
        'usage' => ChatEventKind.usage,
        'done' => ChatEventKind.done,
        'error' => ChatEventKind.error,
        'ping' => ChatEventKind.ping,
        _ => ChatEventKind.unknown,
      };

  static String? _codeOf(Object? value) {
    if (value == null) {
      return null;
    }
    return value is String ? value : value.toString();
  }

  static int? _intOf(Object? value) {
    if (value is int) {
      return value;
    }
    if (value is num) {
      return value.toInt();
    }
    if (value is String) {
      return int.tryParse(value);
    }
    return null;
  }
}

/// 错误码 → 用户可读文案（**不显示原始错误串**）。
String describeChatError(String? code) => switch (code) {
      'UNAUTHORIZED' => '还没填 API Key / 套餐令牌：请在设置里填写后重试',
      'TOKEN_EXPIRED' => '登录已过期，请重新登录',
      'QUOTA_EXCEEDED' => '套餐已到期或额度用完',
      'RATE_LIMITED' => '请求太频繁，请稍后再试',
      'CONCURRENCY_LIMIT' => '上一条回复还没结束，请先停止再发',
      'MESSAGE_TOO_LARGE' => '这一条内容太长了，请精简后再发',
      'UPSTREAM_TIMEOUT' => '上游响应超时，请重试',
      'UPSTREAM_ERROR' => '模型服务暂时不可用，请稍后重试',
      'CANCELLED' => '已停止生成',
      _ => '出错了，请稍后重试',
    };

/// 一次对话的运行时诊断（**内存对象**，供界面读取；不打印日志）。
///
/// 记录六个关键节点：send 调用 / handle 返回值 / 回调注册 / 回调触发 / 事件累计 / 流结束。
/// 任何异常都写进 [lastError]，**不静默吞掉**。**绝不包含 Key 明文**。
class ChatDiagnostics {
  bool sendCalled = false;
  String channel = '';
  String model = '';
  String baseUrl = '';
  String protocol = '';
  int? handle;
  bool callbackRegistered = false;
  String? callbackError;
  int eventsReceived = 0;
  String? lastEventKind;
  String? lastError;
  DateTime? startedAt;
  DateTime? finishedAt;

  /// 界面横幅（3 秒无事件时显示）。
  String get banner =>
      '对话没有启动 · 通道=$channel · handle=${handle ?? "-"} · '
      '回调注册=${callbackRegistered ? "成功" : "失败${callbackError == null ? "" : "($callbackError)"}"} · '
      '已收到事件=$eventsReceived · 最后错误=${lastError ?? "无"}';

  /// 复制给开发者的完整诊断（不含任何凭据）。
  String toClipboardText() => <String>[
        'send_called=$sendCalled',
        'channel=$channel',
        'model=$model',
        'base_url=$baseUrl',
        'protocol=$protocol',
        'handle=${handle ?? "-"}',
        'callback_registered=$callbackRegistered',
        'callback_error=${callbackError ?? "-"}',
        'events_received=$eventsReceived',
        'last_event=${lastEventKind ?? "-"}',
        'last_error=${lastError ?? "-"}',
        'started_at=${startedAt?.toIso8601String() ?? "-"}',
        'finished_at=${finishedAt?.toIso8601String() ?? "-"}',
      ].join(' | ');
}
/// 一次对话的句柄（事件流 + 取消）。
class ChatStream {
  ChatStream._(this._events, this._handle, this._backend, this._onClose, this.diagnostics);

  /// 本轮对话的诊断（界面失败横幅/复制按钮用）。
  final ChatDiagnostics diagnostics;

  final Stream<ChatEvent> _events;
  final int _handle;
  final ChatBackend _backend;
  final void Function() _onClose;

  Stream<ChatEvent> get events => _events;

  /// 停止生成（不再消费上游；幂等）。
  void cancel() {
    if (_handle > 0) {
      _backend.cancel(_handle);
    }
    _onClose();
  }
}

/// FFI 后端（可替换：测试用 fake，不需要真 DLL）。
abstract class ChatBackend {
  /// 发起流；`onChunk` 由后端在收到每个 Chunk 时调用（返回 true 表示继续）。
  int start(String requestJson, bool Function(String chunkJson) onChunk);

  void cancel(int handle);
}

/// 真 FFI 后端：`NativeCallable.listener` + `yunong_chat_stream/cancel`。
class FfiChatBackend implements ChatBackend {
  FfiChatBackend() : _lib = CoreLibrary.openCoreLibrary() {
    _stream = _lib.lookupFunction<_StreamNative, _StreamDart>('yunong_chat_stream');
    _cancelFn = _lib.lookupFunction<_CancelNative, _CancelDart>('yunong_chat_cancel');
  }

  final DynamicLibrary _lib;
  late final _StreamDart _stream;
  late final _CancelDart _cancelFn;

  /// 每个流一个 listener（端口 id 通过 `user` 传给 Rust）。
  final Map<int, ({NativeCallable<_ChunkCbNative> callable, void Function(String) onChunk})>
      _sinks = <int, ({NativeCallable<_ChunkCbNative> callable, void Function(String) onChunk})>{};

  @override
  int start(String requestJson, bool Function(String chunkJson) onChunk) {
    // Rust 侧从任意线程回调 → 必须用 listener（端口投递）而不是 isLeaf/sync
    final callable = NativeCallable<_ChunkCbNative>.listener(
      (Pointer<Utf8> chunkJson, Pointer<Void> user) {
        // 字符串只在回调期间有效：立刻拷成 Dart String
        onChunk(chunkJson.toDartString());
        debugPrint("[chat][4] callback fired");
      },
    );
    final request = requestJson.toNativeUtf8();
    final dbPath = ''.toNativeUtf8();
    try {
      // 用自己的指针当 user：只做标识，不跨 isolate 解引用
      final handle = _stream(request, dbPath, callable.nativeFunction, Pointer.fromAddress(0));
      debugPrint("[chat][2] yunong_chat_stream handle=$handle");
      if (handle > 0) {
        _sinks[handle] = (callable: callable, onChunk: onChunk);
      } else {
        callable.close();
      }
      return handle;
    } finally {
      calloc.free(request);
      calloc.free(dbPath);
    }
  }

  @override
  void cancel(int handle) {
    _cancelFn(handle);
    _sinks.remove(handle)?.callable.close();
  }
}

// listener 只能返回 void（取消走 yunong_chat_cancel，而不是回调返回值）
typedef _ChunkCbNative = Void Function(Pointer<Utf8>, Pointer<Void>);
typedef _StreamNative = Int64 Function(
    Pointer<Utf8>, Pointer<Utf8>, Pointer<NativeFunction<_ChunkCbNative>>, Pointer<Void>);
typedef _StreamDart = int Function(
    Pointer<Utf8>, Pointer<Utf8>, Pointer<NativeFunction<_ChunkCbNative>>, Pointer<Void>);
typedef _CancelNative = Int32 Function(Int64);
typedef _CancelDart = int Function(int);

/// 对话服务：UI 只依赖它。
class ChatService {
  ChatService({ChatBackend? backend}) : _backend = backend ?? FfiChatBackend();

  final ChatBackend _backend;

  /// 发起一次流式对话。
  ChatStream send(ChatRequestDto request) {
    final diag = ChatDiagnostics()
      ..sendCalled = true
      ..channel = request.channel.wire
      ..model = request.model
      ..baseUrl = request.baseUrl ?? '(空)'
      ..protocol = request.protocol.wire
      ..startedAt = DateTime.now();
    final controller = StreamController<ChatEvent>();
    void close() {
      if (!controller.isClosed) {
        unawaited(controller.close());
      }
    }

    var handle = 0;
    try {
      handle = _backend.start(request.toJson(), (String chunkJson) {
        final event = ChatEvent.fromJsonString(chunkJson);
        diag.eventsReceived += 1;
        diag.lastEventKind = event.kind.name;
        if (event.kind == ChatEventKind.error) {
          diag.lastError = event.errorCode ?? event.errorMessage ?? 'error';
        }
        if (!controller.isClosed) {
          controller.add(event);
        }
        if (event.isTerminal) {
          diag.finishedAt = DateTime.now();
          if (handle > 0) {
            _backend.cancel(handle);
          }
          close();
          return false;
        }
        return true;
      });
      diag.callbackRegistered = handle > 0;
    } on Object catch (err) {
      diag.callbackRegistered = false;
      diag.callbackError = err.runtimeType.toString();
      // 不静默：任何异常都进诊断与 UI
      diag.lastError = '${err.runtimeType}: $err';
      diag.finishedAt = DateTime.now();
      if (!controller.isClosed) {
        controller.add(ChatEvent(
          kind: ChatEventKind.error,
          errorCode: 'INTERNAL',
          errorMessage: diag.lastError,
        ));
      }
      close();
      return ChatStream._(controller.stream, 0, _backend, close, diag);
    } finally {
      diag.handle = handle;
    }

    if (handle <= 0) {
      diag.lastError ??= 'yunong_chat_stream 返回 $handle（入口被拒）';
      diag.finishedAt = DateTime.now();
      if (!controller.isClosed) {
        controller.add(const ChatEvent(
          kind: ChatEventKind.error,
          errorCode: 'INTERNAL',
          errorMessage: '端侧核心未就绪',
        ));
      }
      close();
    }
    return ChatStream._(controller.stream, handle, _backend, close, diag);
  }
}