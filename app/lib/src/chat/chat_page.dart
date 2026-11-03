// 会话页（中性样式；视觉留待 designer 选型后换皮）。
//
// 结构刻意保持"可换皮"：只依赖 `Theme.of(context)` 的语义色，不写死配色/圆角；
// 消息列表与输入区拆成独立 widget，后续整体换主题不需要改逻辑。

import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../theme/glass_panel.dart';
import '../theme/yunong_tokens.dart';

import '../core/app_paths.dart';
import '../core/auth_service.dart';
import '../core/chat_service.dart';
import '../core/local_search_service.dart';
import '../core/memory_service.dart';
import '../core/search_service.dart';
import '../core/secret_service.dart';
import '../memory/memory_page.dart';
import '../core/build_info.dart';
import 'chat_settings_section.dart';

// `ChatSettings` 已随设置面板搬到 `chat_settings_section.dart`；这里 re-export，
// 保持既有调用方/测试的 `import chat_page.dart` 不变（纯搬迁，行为不变）。
export 'chat_settings_section.dart' show ChatSettings;
import 'chat_top_bar.dart';

/// 会话设置（MVP：密钥只在内存里，进程退出即失效）。
///
/// 下一步（t34 之后）：接 Rust keystore 的 `yunong_set/get_secret`，
/// 让 Key 落到系统安全存储、永不回显。本轮的 UI 已经按"不回显"设计。
/// 一条气泡。
class ChatBubble {
  ChatBubble({required this.fromUser, required this.text});

  final bool fromUser;
  String text;
}

/// 会话页。
class ChatPage extends StatefulWidget {
  const ChatPage({
    super.key,
    this.service,
    this.settings,
    this.chatSettings,
    this.secretService,
    this.searchService,
    this.localSearchService,
    this.memoryService,
    this.keyValidator,
    this.authService,
    this.dbPath = '',
  });

  final ChatService? service;
  final ChatSettings? settings;

  /// 共享状态源（与设置页同一个实例）：设置页改完这里立即生效
  final ChatSettingsController? chatSettings;
  final SecretService? secretService;
  final SearchService? searchService;
  final LocalSearchService? localSearchService;
  final MemoryService? memoryService;

  /// 保存前的 Key 轻校验（默认真的请求上游 `/models`；测试可注入）。
  /// 返回 null = 可用；否则返回可读错误文案。
  final Future<String?> Function(String apiKey)? keyValidator;

  final AuthService? authService;
  final String dbPath;

  @override
  State<ChatPage> createState() => ChatPageState();
}

/// 公开（而非 `_`）只为 `@visibleForTesting` 的 getter 能被测试读取。
class ChatPageState extends State<ChatPage> {
  late final ChatService _service = widget.service ?? ChatService();
  late final ChatSettings _settings =
      widget.settings ?? widget.chatSettings?.settings ?? ChatSettings();
  late final SecretService _secrets =
      widget.secretService ?? SecretService(dbPath: widget.dbPath);
  SecretStatus _keyStatus = const SecretStatus(set: false, masked: '');
  /// 最近一次「取 Key 状态」失败的原因（诊断可观测；为空 = 没失败过）。
  String? _keyStatusError;

  /// 测试用：当前 Key 状态文案（`未设置` / 打码串）。
  ///
  /// 不走 Tooltip 断言——前三次失败都是读错了 Tooltip（另一个控件或 offstage 文案）。
  @visibleForTesting
  String get keyStatusLabel => _keyStatus.display;

  /// 测试用：最近一次取 Key 状态失败的原因（正常时应为空）。
  @visibleForTesting
  String? get keyStatusError => _keyStatusError;

  /// 测试用：通道就绪判定（含协议不支持的提前拦截）。
  @visibleForTesting
  ({bool ready, String reason, String code}) get channelStateForTest => _channelState;
  late final AuthService _auth = widget.authService ?? AuthService();
  late final SearchService _search =
      widget.searchService ?? SearchService(dbPath: widget.dbPath);
  SearchStatus _searchStatus = const SearchStatus.unknown();
  late final LocalSearchService _localSearch =
      widget.localSearchService ?? LocalSearchService(dbPath: widget.dbPath);
  late final MemoryService _memory =
      widget.memoryService ?? MemoryService(dbPath: widget.dbPath);
  bool _localPanelOpen = false;
  bool _localBusy = false;
  String? _localError;
  List<LocalSearchHit> _localHits = const <LocalSearchHit>[];
  final TextEditingController _localQuery = TextEditingController();
  final TextEditingController _input = TextEditingController();
  final ScrollController _scroll = ScrollController();

  final List<ChatBubble> _bubbles = <ChatBubble>[];
  ChatStream? _stream;
  bool _streaming = false;
  String? _error;
  /// 首字慢的非致命提示（不取消流）
  String? _slowHint;

  /// 最近一轮对话的诊断（3 秒无事件时显示横幅 + 可复制）。
  ChatDiagnostics? _diagnostics;
  int? _promptTokens;
  int? _completionTokens;
  String _channel = 'byok';

  @override
  void initState() {
    super.initState();
    _channel = _settings.channel;
    // 共享设置源变化（例如在设置页保存了 Key）→ 立即刷新本页状态
    widget.chatSettings?.addListener(_onSharedSettingsChanged);
    unawaited(_refreshKeyStatus());
    unawaited(_refreshSearchStatus());
  }

  @override
  void dispose() {
    _stream?.cancel();
    widget.chatSettings?.removeListener(_onSharedSettingsChanged);
    _input.dispose();
    _scroll.dispose();
    super.dispose();
  }

  void _onSharedSettingsChanged() {
    unawaited(_refreshKeyStatus());
    if (mounted) {
      setState(() {});
    }
  }

  void _scrollToBottom() {
    if (!_scroll.hasClients) {
      return;
    }
    _scroll.jumpTo(_scroll.position.maxScrollExtent);
  }

  /// 通道是否就绪 + 未就绪原因（**“就绪”不能是假的**）。
  ///
  /// - BYOK：必须已在安全存储里保存 Key；
  /// - 套餐：必须有访问令牌；
  /// - 其它：未选择通道。
  ({bool ready, String reason, String code}) get _channelState {
    if (_channel == 'byok') {
      // t94：DeepSeek/OpenAI 只提供 OpenAI 兼容接口，没有 Anthropic Messages API。
      // 提前拦住，别让用户看到笼统的 UPSTREAM_ERROR。
      if (_settings.protocol == 'anthropic_messages' && isOpenAiOnlyUpstream(_settings.baseUrl)) {
        return (
          ready: false,
          reason: '该上游不支持 Anthropic Messages 协议，请改用 OpenAI Completions',
          code: 'PROTOCOL_UNSUPPORTED',
        );
      }
      if (_keyStatus.usable) {
        return (ready: true, reason: '', code: '');
      }
      if (_keyStatus.broken) {
        return (ready: false, reason: '本地凭据已损坏，请重新填写 Key', code: 'SECRET_BROKEN');
      }
      return (ready: false, reason: '未设置 API Key：到设置页填入', code: 'MISSING_API_KEY');
    }
    if (_channel == 'cloud') {
      if (_auth.signedIn || _settings.cloudToken.isNotEmpty) {
        return (ready: true, reason: '', code: '');
      }
      return (ready: false, reason: '请先登录套餐账号', code: 'NOT_SIGNED_IN');
    }
    return (ready: false, reason: '未选择对话通道', code: 'NO_CHANNEL');
  }
  /// Enter 发送 / Shift+Enter 换行；输入法组合（拼音未上屏）期间不发送。
  KeyEventResult _handleInputKey(KeyEvent event) {
    if (event is! KeyDownEvent) {
      return KeyEventResult.ignored;
    }
    final key = event.logicalKey;
    if (key != LogicalKeyboardKey.enter && key != LogicalKeyboardKey.numpadEnter) {
      return KeyEventResult.ignored;
    }
    // Shift+Enter：让 TextField 自己插入换行
    if (HardwareKeyboard.instance.isShiftPressed) {
      return KeyEventResult.ignored;
    }
    // 输入法组合中：拼音/候选还没上屏，回车是"确认候选"，绝不能当发送
    if (_input.value.composing.isValid) {
      return KeyEventResult.ignored;
    }
    _send();
    return KeyEventResult.handled;
  }
  Future<void> _send() async {
    final text = _input.text.trim();
    if (text.isEmpty || _streaming) {
      return;
    }
    // 本地拦截：通道没就绪就不发请求（不要“看起来发了”）
    final state = _channelState;
    if (!state.ready) {
      setState(() => _error = state.reason);
      return;
    }
    _input.clear();
    setState(() {
      _bubbles.add(ChatBubble(fromUser: true, text: text));
      final reply = ChatBubble(fromUser: false, text: '');
      _bubbles.add(reply);
      _streaming = true;
      _error = null;
      _promptTokens = null;
      _completionTokens = null;
    });
    _scrollToBottom();

    final history = _bubbles
        .where((ChatBubble bubble) => bubble.text.isNotEmpty)
        .map((ChatBubble bubble) => bubble.fromUser
            ? ChatTurn.user(bubble.text)
            : ChatTurn.assistant(bubble.text))
        .toList();

    final request = ChatRequestDto(
      channel: _channel == 'cloud' ? ChatChannel.cloud : ChatChannel.byok,
      model: _settings.model,
      protocol: ChatProtocol.values.firstWhere(
        (ChatProtocol value) => value.wire == _settings.protocol,
        orElse: () => ChatProtocol.completions,
      ),
      messages: history,
      // BYOK：只传 key 名，明文由 Rust 从安全存储取（Dart 不见明文）
      baseUrl: _settings.baseUrl,
      secretKey: _channel == 'byok' ? SecretService.byokKeyName : null,
      // Rust 侧把 data_dir 当**目录**用（secrets 子目录）：必须传目录，不能传 .db 文件路径，
      // 否则 set_secret（按 db_path 的父目录）与 chat（按 data_dir）各看一个目录 → 读不到 Key（t54 之后的新根因）。
      dataDir: _dataDir,
      cloudToken: _settings.cloudToken,
    );

    final ChatStream stream;
    try {
      stream = _service.send(request);
    } on Object catch (err) {
      setState(() {
        _streaming = false;
        _error = '端侧核心调用失败：' + describeChatError('INTERNAL') + '（' + err.runtimeType.toString() + '）';
      });
      return;
    }
    _stream = stream;
    // 无事件兜底：Rust 回调 ABI 不匹配时事件永远不来，界面就会停在占位三点。
    // P0-1（t56）：失败必须可观测——内存诊断，不依赖 Release 看不了的日志
    _diagnostics = stream.diagnostics;
    var sawEvent = false;
    // P0-1（t79）：**只提示，不取消**。DeepSeek/思考模型首个 token 常 >3s；
    // 之前 3s 看门狗直接 cancel → handle=1、callback_registered=true、events=0、无错误，
    // 正是 owner 看到的“默默被杀”。取消只由用户点「停止」触发。
    final watchdog = Timer(const Duration(seconds: 5), () {
      if (!mounted || sawEvent) {
        return;
      }
      setState(() {
        _slowHint = '流已启动，但 5 秒内还没收到任何数据（handle=${stream.diagnostics.handle}），仍在等待…';
      });
    });
    stream.events.listen(
      (ChatEvent event) {
        sawEvent = true;
        watchdog.cancel();
        if (_slowHint != null) {
          setState(() => _slowHint = null);
        }
        if (!mounted) {
          return;
        }
        setState(() {
          switch (event.kind) {
            case ChatEventKind.delta:
              _bubbles.last.text += event.text ?? '';
            case ChatEventKind.usage:
              _promptTokens = event.promptTokens ?? _promptTokens;
              _completionTokens = event.completionTokens ?? _completionTokens;
            case ChatEventKind.error:
              _error = describeChatError(event.errorCode);
            case ChatEventKind.done:
              break;
            default:
              break;
          }
        });
        _scrollToBottom();
      },
      onDone: () {
        watchdog.cancel();
        if (!mounted) {
          return;
        }
        setState(() {
          _streaming = false;
          _stream = null;
        });
      },
      onError: (Object _) {
        if (!mounted) {
          return;
        }
        setState(() {
          _streaming = false;
          _stream = null;
          _error = '出错了，请稍后重试';
        });
      },
    );
  }

  void _stop() {
    _stream?.cancel();
    setState(() {
      _streaming = false;
      _stream = null;
    });
  }

  Future<void> _signInCloud() async {
    final result = await showDialog<({String email, String password})>(
      context: context,
      builder: (BuildContext context) => const CloudLoginDialog(),
    );
    if (result == null || !mounted) {
      return;
    }
    try {
      await _auth.signIn(email: result.email, password: result.password);
      if (!mounted) {
        return;
      }
      setState(() {
        _channel = 'cloud';
        _error = null;
      });
      ScaffoldMessenger.of(context).showSnackBar(
        SnackBar(content: Text('已登录套餐账号 ${_auth.maskedEmail}')),
      );
    } on AuthException catch (failure) {
      _showError(failure.userHint);
    } on Object {
      _showError('登录失败，请稍后重试');
    }
  }

  Future<void> _signOutCloud() async {
    final confirmed = await showDialog<bool>(
      context: context,
      builder: (BuildContext context) => AlertDialog(
        backgroundColor: Theme.of(context).colorScheme.surface,
        surfaceTintColor: Colors.transparent,
        title: const Text('退出套餐账号？'),
        content: Text('将退出 ${_auth.maskedEmail}，本机令牌会被清除。'),
        actions: <Widget>[
          TextButton(onPressed: () => Navigator.of(context).pop(false), child: const Text('取消')),
          FilledButton(onPressed: () => Navigator.of(context).pop(true), child: const Text('退出')),
        ],
      ),
    );
    if (confirmed != true) {
      return;
    }
    await _auth.signOut();
    if (!mounted) {
      return;
    }
    setState(() {
      if (_channel == 'cloud') {
        _channel = 'byok';
      }
    });
    ScaffoldMessenger.of(context).showSnackBar(
      const SnackBar(content: Text('已退出套餐账号')),
    );
  }
  /// 对话诊断（始终可达）：显示最近一轮的观测数据并支持复制。
  ///
  /// P0-1 观测口径：`delta_count` / `last_delta_len` / `merged_chars` 能直接回答
  /// “收到几条 delta、第几条之后停止、是否收到 done/error”。
  Future<void> _showDiagnostics() async {
    final diag = _diagnostics;
    final keyLine =
        'key=`${_keyStatus.masked.isEmpty ? '未设置' : _keyStatus.masked} | protocol=`${_settings.protocol}'
        ' | key_status_error=`${_keyStatusError ?? ''}'
        ' | build=`${BuildInfo.label}';
    final text = keyLine + '\n' + (diag == null ? '还没有发起过对话' : diag.toClipboardText());
    if (!mounted) {
      return;
    }
    await showDialog<void>(
      context: context,
      builder: (BuildContext context) => AlertDialog(
        title: const Text('对话诊断'),
        scrollable: true,
        content: SelectableText(text, style: const TextStyle(fontSize: 12)),
        actions: <Widget>[
          TextButton(
            onPressed: () async {
              await Clipboard.setData(ClipboardData(text: text));
              if (context.mounted) {
                Navigator.of(context).pop();
              }
            },
            child: const Text('复制诊断'),
          ),
          FilledButton(onPressed: () => Navigator.of(context).pop(), child: const Text('关闭')),
        ],
      ),
    );
  }
  /// 查询搜索来源：未配置时 UI **必须**常驻提示（禁止静默降级）。
  /// 本地资料检索：结果**只来自本机**（与联网搜索区分，不混淆）。
  Future<void> _runLocalSearch() async {
    final query = _localQuery.text.trim();
    if (query.isEmpty) {
      return;
    }
    setState(() {
      _localBusy = true;
      _localError = null;
      _localHits = const <LocalSearchHit>[];
    });
    try {
      final hits = await _localSearch.search(query);
      if (mounted) {
        setState(() => _localHits = hits);
      }
    } on SearchQueryTooLong catch (tooLong) {
      if (mounted) {
        setState(() => _localError = tooLong.userHint);
      }
    } on LocalSearchUnavailable catch (unavailable) {
      if (mounted) {
        setState(() => _localError = unavailable.userHint);
      }
    } on Object {
      if (mounted) {
        setState(() => _localError = '本地搜索失败，请稍后重试');
      }
    } finally {
      if (mounted) {
        setState(() => _localBusy = false);
      }
    }
  }

  /// 把本地资料作为**引用**插入输入框（带来源标题，模型能看出这是资料不是指令）。
  void _insertCitation(LocalSearchHit hit) {
    final existing = _input.text.trim();
    _input.text =
        existing.isEmpty ? hit.citation : '${hit.citation}\n\n$existing';
    setState(() => _localPanelOpen = false);
    _localHits = const <LocalSearchHit>[];
    _localQuery.clear();
  }

  Future<void> _openMemory() async {
    await Navigator.of(context).push<void>(
      MaterialPageRoute<void>(
          builder: (BuildContext context) => MemoryPage(service: _memory)),
    );
  }

  Future<void> _refreshSearchStatus() async {
    try {
      final protocol = _settings.protocol;
      final status =
          await _search.status(channel: _channel, protocol: protocol);
      if (mounted) {
        setState(() => _searchStatus = status);
      }
    } on Object {
      // 核心库不可用（如测试环境）：诚实显示“未配置”，不假装可用
      if (mounted) {
        setState(() => _searchStatus = const SearchStatus.unknown());
      }
    }
  }

  Future<void> _refreshKeyStatus() async {
    try {
      final status = await _secrets.status(SecretService.byokKeyName);
      if (mounted) {
        setState(() => _keyStatus = status);
      }
      if (status.broken) {
        _showError('本地凭据已损坏，请重新填写 Key');
      }
    } on SecretException catch (failure) {
      if (mounted) {
        setState(() => _error = failure.userHint);
      }
    } on Object catch (failure) {
      // 读取失败**不再重置为“未设置”**（那会把用户的“已配置”抹掉、连带禁用发送），
      // 保留上一次的已知状态，只把原因记录到诊断里。
      _keyStatusError = '${failure.runtimeType}: $failure';
      debugPrint('[chat] key status refresh failed: $_keyStatusError');
    }
  }

  /// 轻量校验：请求一次 `/models`（OpenAI 兼容），8s 超时。
  ///
  /// 返回 null = 可用；否则返回可读错误（**不显示原始错误串**）。

  void _showError(String message) {
    if (!mounted) {
      return;
    }
    setState(() => _error = message);
    ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text(message)));
  }

  /// 保存 Key：**先校验、再落盘、再回读**，任一失败都不假装成功。
  /// 端侧数据**目录**（Rust `data_dir`）：由 `AppPaths.dataDirOf` 统一推导。
  String get _dataDir => AppPaths.dataDirOf(widget.dbPath);

  Future<void> _deleteKey() async {
    try {
      await _secrets.delete(SecretService.byokKeyName);
      await _refreshKeyStatus();
    } on SecretException catch (failure) {
      if (mounted) {
        setState(() => _error = failure.userHint);
      }
    }
  }


  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Column(
      children: <Widget>[
        ChatTopBar(
          channel: _channel,
          onChannelChanged: (String value) {
            setState(() => _channel = value);
            unawaited(_refreshSearchStatus());
          },
          statusLabel: _streaming
              ? '正在生成…'
              : (_channelState.ready ? '就绪' : _channelState.reason),
          searchLabel: _searchStatus.display,
          keyLabel: _keyStatus.display,
          keyDegraded: _keyStatus.degraded,
          signedIn: _auth.signedIn,
          accountLabel: _auth.signedIn ? _auth.maskedEmail : '未登录',
          onLocalSearch: () => setState(() => _localPanelOpen = !_localPanelOpen),
          onMemory: () => unawaited(_openMemory()),
          onSignIn: () => unawaited(_signInCloud()),
          onSignOut: () => unawaited(_signOutCloud()),
          onDiagnostics: () => unawaited(_showDiagnostics()),
          onDeleteKey: _keyStatus.set ? () => unawaited(_deleteKey()) : null,
        ),
        if (_keyStatus.degraded)
          Padding(
            padding: const EdgeInsets.fromLTRB(12, 8, 12, 0),
            child: Text(
              'Key ${_keyStatus.degradedHint}',
              style: theme.textTheme.bodySmall
                  ?.copyWith(color: theme.colorScheme.tertiary),
            ),
          ),
        if (_slowHint != null)
          Padding(
            padding: const EdgeInsets.fromLTRB(12, 0, 12, 4),
            child: Text(_slowHint!, style: theme.textTheme.bodySmall),
          ),
        if (_error != null)
          Padding(
            padding: const EdgeInsets.fromLTRB(12, 8, 12, 0),
            child: Row(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: <Widget>[
                Expanded(
                  child: Text(
                    _error!,
                    style: theme.textTheme.bodySmall
                        ?.copyWith(color: theme.colorScheme.error),
                  ),
                ),
                if (_diagnostics != null)
                  TextButton.icon(
                    onPressed: () async {
                      await Clipboard.setData(
                        ClipboardData(text: _diagnostics!.toClipboardText()),
                      );
                      if (context.mounted) {
                        ScaffoldMessenger.of(context).showSnackBar(
                          const SnackBar(content: Text('诊断已复制，可直接粘贴给开发者')),
                        );
                      }
                    },
                    icon: const Icon(Icons.copy, size: 16),
                    label: const Text('复制诊断'),
                  ),
              ],
            ),
          ),
        if (!_searchStatus.available)
          Container(
            width: double.infinity,
            color: theme.colorScheme.surfaceContainerHighest,
            padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 6),
            child: Text(
              _searchStatus.hint.isEmpty
                  ? '未配置联网搜索，模型无法获取实时信息'
                  : _searchStatus.hint,
              style: theme.textTheme.bodySmall
                  ?.copyWith(color: theme.colorScheme.tertiary),
            ),
          ),
        if (_localPanelOpen)
          Container(
            width: double.infinity,
            margin: const EdgeInsets.fromLTRB(12, 8, 12, 0),
            padding: const EdgeInsets.all(10),
            // Material 包一层：ListTile 的水波纹要画在最近 Material 上，否则会被背景色盖住
            child: Material(
              color: theme.colorScheme.surfaceContainerHighest,
              borderRadius: BorderRadius.circular(8),
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: <Widget>[
                Row(
                  children: <Widget>[
                    Expanded(
                      child: Text(
                        '本地资料（只查这台设备，不联网）',
                        style: theme.textTheme.bodySmall
                            ?.copyWith(fontWeight: FontWeight.w600),
                      ),
                    ),
                    IconButton(
                      tooltip: '关闭本地资料',
                      visualDensity: VisualDensity.compact,
                      onPressed: () => setState(() => _localPanelOpen = false),
                      icon: const Icon(Icons.close, size: 16),
                    ),
                  ],
                ),
                  const SizedBox(height: 6),
                  Row(
                    children: <Widget>[
                      Expanded(
                        child: TextField(
                          controller: _localQuery,
                          decoration: const InputDecoration(
                            hintText: '搜索本机记忆/资料…',
                            isDense: true,
                            border: OutlineInputBorder(),
                          ),
                          onSubmitted: (_) => _runLocalSearch(),
                        ),
                      ),
                      const SizedBox(width: 8),
                      FilledButton(
                        onPressed: _localBusy ? null : _runLocalSearch,
                        child: const Text('搜索'),
                      ),
                    ],
                  ),
                  if (_localError != null)
                    Padding(
                      padding: const EdgeInsets.only(top: 6),
                      child: Text(
                        _localError!,
                        style: theme.textTheme.bodySmall
                            ?.copyWith(color: theme.colorScheme.error),
                      ),
                    ),
                  if (!_localBusy &&
                      _localError == null &&
                      _localHits.isEmpty &&
                      _localQuery.text.trim().isNotEmpty)
                    Padding(
                      padding: const EdgeInsets.only(top: 6),
                      child:
                          Text('本机没有匹配的资料', style: theme.textTheme.bodySmall),
                    ),
                  for (final hit in _localHits)
                    ListTile(
                      dense: true,
                      contentPadding: EdgeInsets.zero,
                      leading: const Icon(Icons.description_outlined, size: 18),
                      title: Text(hit.displayTitle,
                          style: theme.textTheme.bodyMedium),
                      subtitle: Text(hit.snippet,
                          maxLines: 2, overflow: TextOverflow.ellipsis),
                      onTap: () => _insertCitation(hit),
                    ),
                ],
              ),
            ),
          ),
        Expanded(
          child: _bubbles.isEmpty
              ? _EmptyState(onPick: _fillInput)

              : ListView.builder(
                  controller: _scroll,
                  padding: const EdgeInsets.all(12),
                  itemCount: _bubbles.length,
                  itemBuilder: (BuildContext context, int index) {
                    final bubble = _bubbles[index];
                    return _MessageBubble(bubble: bubble);
                  },
                ),
        ),

        // B（深蓝玻璃）下输入条是一块玻璃：这是**全屏第二层也是最后一层** BackdropFilter。
        // A/C 没有玻璃 token，这里退回普通 Padding，不产生任何模糊。
        if (context.tokens.glass != null)
          Padding(
            padding: const EdgeInsets.fromLTRB(12, 4, 12, 12),
            child: GlassPanel(
              role: GlassRole.composer,
              padding: const EdgeInsets.all(8),
              child: _composer(),
            ),
          )
        else
          Padding(
            padding: const EdgeInsets.fromLTRB(12, 4, 12, 12),
            child: _composer(),
          ),
      ],
    );
  }

  /// 把建议 chip 的文案填进输入框（不直接发送——用户还能改）。
  void _fillInput(String text) {
    setState(() {
      _input.text = text;
      _input.selection = TextSelection.collapsed(offset: text.length);
    });
  }

  Widget _composer() {
    return Row(
      children: <Widget>[
        Expanded(
          child: Focus(
            onKeyEvent: (FocusNode node, KeyEvent event) => _handleInputKey(event),
            child: TextField(
              controller: _input,
              minLines: 1,
              maxLines: 4,
              onSubmitted: (_) => _send(),
              decoration: const InputDecoration(
                hintText: '输入消息…',
                isDense: true,
                border: OutlineInputBorder(),
              ),
            ),
          ),
        ),
        const SizedBox(width: 8),
        if (_streaming)
          OutlinedButton.icon(
            onPressed: _stop,
            icon: const Icon(Icons.stop, size: 18),
            label: const Text('停止'),
          )
        else
          FilledButton.icon(
            onPressed: _send,
            icon: const Icon(Icons.send, size: 18),
            label: const Text('发送'),
          ),
      ],
    );
  }
}

/// 空态（三套主题**结构一致**：问候 + 一句副标题 + 建议 chip）。
///
/// 教训（t97）：这里曾经放过一个"深色引文块"金句，只存在于 C 主题。
/// ① 三套主题结构不一致 → 它不是空态设计，而是某主题的独占装饰；
/// ② 空态的任务是"让人知道能干什么"，不是展示金句；
/// ③ 深色墨块视觉重量压过真正的引导（问候 + chip）。
///
/// 现在：**整簇贴着输入框锚定在下方**。删掉装饰后页面不空——重心落在
/// "输入框 + 建议 chip"这两个要操作的对象上，中间的留白是有意的呼吸区。
class _EmptyState extends StatelessWidget {
  const _EmptyState({required this.onPick});

  final ValueChanged<String> onPick;

  static const List<String> _suggestions = <String>[
    '第 8 周哪天最累？',
    '把课表导出成日历',
    '这门课的考核方式',
    '这学期一共几门课？',
  ];

  @override
  Widget build(BuildContext context) {
    final ThemeData theme = Theme.of(context);
    final YunongTokens t = context.tokens;

    return Align(
      alignment: Alignment.bottomCenter,
      child: SingleChildScrollView(
        padding: const EdgeInsets.fromLTRB(24, 16, 24, 10),
        child: ConstrainedBox(
          constraints: const BoxConstraints(maxWidth: 560),
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.center,
            children: <Widget>[
              Text(
                '今天想从哪儿开始？',
                textAlign: TextAlign.center,
                style: theme.textTheme.displaySmall,
              ),
              const SizedBox(height: 8),
              Text(
                '问点什么吧——课表、成绩、校园生活都行。',
                textAlign: TextAlign.center,
                style: theme.textTheme.bodyMedium?.copyWith(color: t.inkSoft),
              ),
              const SizedBox(height: 16),
              // 用 chip 的数量与排布补密度（而不是加装饰）；窄窗自动换行
              Wrap(
                alignment: WrapAlignment.center,
                spacing: 8,
                runSpacing: 8,
                children: <Widget>[
                  for (final String text in _suggestions)
                    _SuggestionChip(label: text, onTap: () => onPick(text)),
                ],
              ),
            ],
          ),
        ),
      ),
    );
  }
}
/// 建议 chip：环形阴影（不占布局）而不是边框。
class _SuggestionChip extends StatelessWidget {
  const _SuggestionChip({required this.label, required this.onTap});

  final String label;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final YunongTokens t = context.tokens;
    return Material(
      color: Colors.transparent,
      child: InkWell(
        onTap: onTap,
        borderRadius: BorderRadius.circular(999),
        child: Container(
          padding: const EdgeInsets.symmetric(horizontal: 13, vertical: 7),
          decoration: BoxDecoration(
            color: t.surface,
            borderRadius: BorderRadius.circular(999),
            boxShadow: ringShadow(t.hairline),
          ),
          child: Text(label, style: Theme.of(context).textTheme.bodySmall),
        ),
      ),
    );
  }
}

class _MessageBubble extends StatelessWidget {
  const _MessageBubble({required this.bubble});

  final ChatBubble bubble;

  @override
  Widget build(BuildContext context) {
    final ThemeData theme = Theme.of(context);
    final YunongTokens t = context.tokens;
    final bool isUser = bubble.fromUser;
    final String text = bubble.text.isEmpty ? '…' : bubble.text;

    // C（书本质感）走"书页"排版：助手回答是正文（无气泡、无背景），
    // 用户提问是右上角的页边批注。其它主题保持聊天气泡。
    if (t.bookLayout) {
      if (isUser) {
        return Align(
          alignment: Alignment.centerRight,
          child: ConstrainedBox(
            constraints: const BoxConstraints(maxWidth: 520),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.end,
              children: <Widget>[
                Text(
                  '你 · 提问',
                  style: theme.textTheme.labelSmall?.copyWith(
                    color: t.inkFaint,
                    letterSpacing: 1.8,
                    fontWeight: FontWeight.w600,
                  ),
                ),
                const SizedBox(height: 6),
                Text(
                  text,
                  textAlign: TextAlign.right,
                  style: theme.textTheme.bodyMedium?.copyWith(
                    fontFamily: t.displayFontFamily,
                    fontStyle: FontStyle.italic,
                    color: t.bodyText,
                  ),
                ),
              ],
            ),
          ),
        );
      }
      return Padding(
        padding: const EdgeInsets.symmetric(vertical: 12),
        child: Text(text, style: theme.textTheme.bodyMedium),
      );
    }

    return Align(
      alignment: isUser ? Alignment.centerRight : Alignment.centerLeft,
      child: Container(
        margin: const EdgeInsets.symmetric(vertical: 4),
        padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 8),
        constraints: const BoxConstraints(maxWidth: 560),
        decoration: BoxDecoration(
          color: isUser
              ? theme.colorScheme.primaryContainer
              : theme.colorScheme.surfaceContainerHighest,
          borderRadius: BorderRadius.circular(t.radiusS),
        ),
        child: Text(text, style: theme.textTheme.bodyMedium),
      ),
    );
  }
}

/// 设置对话框：Key 输入框 `obscureText`，**不回显**已保存的值。

/// 套餐账号登录对话框（邮箱 + 密码；密码不回显）。

/// 只提供 OpenAI 兼容接口的上游（没有 Anthropic Messages API）。
///
/// t94：`anthropic_messages` + 这些上游 = 预期内失败，必须在发送前就提示，
/// 而不是让用户看到一个笼统的 `UPSTREAM_ERROR`。
bool isOpenAiOnlyUpstream(String baseUrl) {
  final String host = Uri.tryParse(baseUrl)?.host.toLowerCase() ?? baseUrl.toLowerCase();
  return host.contains('deepseek') || host.contains('openai') || host.contains('moonshot');
}