// 会话页（中性样式；视觉留待 designer 选型后换皮）。
//
// 结构刻意保持"可换皮"：只依赖 `Theme.of(context)` 的语义色，不写死配色/圆角；
// 消息列表与输入区拆成独立 widget，后续整体换主题不需要改逻辑。

import 'dart:async';
import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../theme/glass_panel.dart';
import '../theme/yunong_tokens.dart';

import '../core/chat_service.dart';
import '../core/local_search_service.dart';
import '../core/memory_service.dart';
import '../core/search_service.dart';
import '../core/secret_service.dart';
import '../memory/memory_page.dart';

/// 会话设置（MVP：密钥只在内存里，进程退出即失效）。
///
/// 下一步（t34 之后）：接 Rust keystore 的 `yunong_set/get_secret`，
/// 让 Key 落到系统安全存储、永不回显。本轮的 UI 已经按"不回显"设计。
class ChatSettings {
  String channel = 'byok';

  /// 上游提供方（架构预留：加 OpenAI / 自建网关只需加枚举值）。
  String providerId = 'deepseek';

  /// 该提供方的 OpenAI 兼容 base url（BYOK 直连用）。
  String baseUrl = 'https://api.deepseek.com';
  String byokKey = '';
  String cloudToken = '';
  String model = 'deepseek-chat';
  String protocol = 'openai_completions';
}

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
    this.secretService,
    this.searchService,
    this.localSearchService,
    this.memoryService,
    this.keyValidator,
    this.dbPath = '',
  });

  final ChatService? service;
  final ChatSettings? settings;
  final SecretService? secretService;
  final SearchService? searchService;
  final LocalSearchService? localSearchService;
  final MemoryService? memoryService;

  /// 保存前的 Key 轻校验（默认真的请求上游 `/models`；测试可注入）。
  /// 返回 null = 可用；否则返回可读错误文案。
  final Future<String?> Function(String apiKey)? keyValidator;
  final String dbPath;

  @override
  State<ChatPage> createState() => _ChatPageState();
}

class _ChatPageState extends State<ChatPage> {
  late final ChatService _service = widget.service ?? ChatService();
  late final ChatSettings _settings = widget.settings ?? ChatSettings();
  late final SecretService _secrets =
      widget.secretService ?? SecretService(dbPath: widget.dbPath);
  SecretStatus _keyStatus = const SecretStatus(set: false, masked: '');
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

  /// 最近一轮对话的诊断（3 秒无事件时显示横幅 + 可复制）。
  ChatDiagnostics? _diagnostics;
  int? _promptTokens;
  int? _completionTokens;
  String _channel = 'byok';

  @override
  void initState() {
    super.initState();
    _channel = _settings.channel;
    unawaited(_refreshKeyStatus());
    unawaited(_refreshSearchStatus());
  }

  @override
  void dispose() {
    _stream?.cancel();
    _input.dispose();
    _scroll.dispose();
    super.dispose();
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
      if (_keyStatus.set) {
        return (ready: true, reason: '', code: '');
      }
      return (ready: false, reason: '未设置 API Key：点右上角设置填入', code: 'MISSING_API_KEY');
    }
    if (_channel == 'cloud') {
      if (_settings.cloudToken.isNotEmpty) {
        return (ready: true, reason: '', code: '');
      }
      return (ready: false, reason: '套餐未登录：请先在设置里填写访问令牌', code: 'MISSING_ACCESS_TOKEN');
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
      dataDir: widget.dbPath,
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
    final watchdog = Timer(const Duration(seconds: 3), () {
      if (!mounted || sawEvent || !_streaming) {
        return;
      }
      stream.cancel();
      setState(() {
        _streaming = false;
        _stream = null;
        _error = stream.diagnostics.banner;
      });
    });
    stream.events.listen(
      (ChatEvent event) {
        sawEvent = true;
        watchdog.cancel();
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
    } on SecretException catch (failure) {
      if (mounted) {
        setState(() => _error = failure.userHint);
      }
    } on Object {
      // 核心库不可用（例如 widget 测试环境没有 DLL）：保持“未设置”，不打扰用户
      if (mounted) {
        setState(() => _keyStatus = const SecretStatus(set: false, masked: ''));
      }
    }
  }

  /// 轻量校验：请求一次 `/models`（OpenAI 兼容），8s 超时。
  ///
  /// 返回 null = 可用；否则返回可读错误（**不显示原始错误串**）。
  static Future<String?> _validateByokKey(String apiKey) async {
    final client = HttpClient()..connectionTimeout = const Duration(seconds: 8);
    try {
      final request = await client
          .getUrl(Uri.parse('https://api.deepseek.com/models'))
          .timeout(const Duration(seconds: 8));
      request.headers.set('Authorization', 'Bearer $apiKey');
      final response = await request.close().timeout(const Duration(seconds: 8));
      await response.drain<void>();
      if (response.statusCode == 200) {
        return null;
      }
      if (response.statusCode == 401 || response.statusCode == 403) {
        return 'Key 无效（上游返回 ${response.statusCode}）';
      }
      return '上游暂不可用（HTTP ${response.statusCode}），请稍后重试';
    } on Object {
      return '网络不可达，无法校验 Key（请检查网络后重试）';
    } finally {
      client.close(force: true);
    }
  }

  void _showError(String message) {
    if (!mounted) {
      return;
    }
    setState(() => _error = message);
    ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text(message)));
  }

  /// 保存 Key：**先校验、再落盘、再回读**，任一失败都不假装成功。
  Future<void> _saveKey(String apiKey) async {
    final validator = widget.keyValidator ?? _validateByokKey;
    final reason = await validator(apiKey);
    if (reason != null) {
      _showError(reason);
      return;
    }
    try {
      final saved = await _secrets.save(SecretService.byokKeyName, apiKey);
      if (!saved.set) {
        _showError('保存失败：安全存储没有确认写入');
        return;
      }
      final status = await _secrets.status(SecretService.byokKeyName);
      if (!status.set || status.masked.isEmpty) {
        _showError('保存后回读失败：Key 没有真正落盘');
        return;
      }
      if (mounted) {
        setState(() {
          _keyStatus = status;
          _error = null;
        });
      }
      if (mounted) {
        ScaffoldMessenger.of(context).showSnackBar(
          SnackBar(content: Text('已保存 ${status.masked}')),
        );
      }
    } on SecretException catch (failure) {
      _showError(failure.userHint);
    } on Object {
      _showError('保存失败：端侧核心不可用');
    }
  }
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

  Future<void> _openSettings() async {
    await _refreshKeyStatus();
    if (!mounted) {
      return;
    }
    final result = await showDialog<ChatSettings>(
      context: context,
      builder: (BuildContext context) => _SettingsDialog(
        settings: _settings,
        searchLabel: _searchStatus.display,
        searchHint: _searchStatus.available ? '' : _searchStatus.hint,
      ),
    );
    if (result != null && mounted) {
      if (result.byokKey.isNotEmpty) {
        // 明文只在这一行里出现，交给 _saveKey 后立刻清掉
        final apiKey = result.byokKey;
        result.byokKey = '';
        await _saveKey(apiKey);
        if (!mounted) {
          return;
        }
      }      setState(() {
        _settings.cloudToken = result.cloudToken;
        _settings.model = result.model;
        _settings.protocol = result.protocol;
        _channel = result.channel;
      });
    }
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Column(
      children: <Widget>[
        Padding(
          padding: const EdgeInsets.fromLTRB(12, 8, 12, 0),
          child: Row(
            children: <Widget>[
              SegmentedButton<String>(
                segments: const <ButtonSegment<String>>[
                  ButtonSegment<String>(value: 'byok', label: Text('BYOK 直连')),
                  ButtonSegment<String>(value: 'cloud', label: Text('套餐通道')),
                ],
                selected: <String>{_channel},
                onSelectionChanged: (Set<String> value) {
                  setState(() => _channel = value.first);
                  unawaited(_refreshSearchStatus());
                },
              ),
              const SizedBox(width: 12),
              Expanded(
                child: Text(
                  _streaming
                      ? '正在生成…'
                      : (_channelState.ready ? '就绪' : _channelState.reason),
                  style: theme.textTheme.bodySmall,
                ),
              ),
              if (_promptTokens != null || _completionTokens != null)
                Text(
                  '用量 ${_promptTokens ?? '-'} / ${_completionTokens ?? '-'}',
                  style: theme.textTheme.bodySmall,
                ),
              Text('搜索 ${_searchStatus.display}',
                  style: theme.textTheme.bodySmall),
              if (_channel == 'byok')
                Text('Key ${_keyStatus.display}',
                    style: theme.textTheme.bodySmall),
              if (_channel == 'byok' && _keyStatus.set)
                IconButton(
                  tooltip: '删除已保存的 Key',
                  onPressed: _deleteKey,
                  icon: const Icon(Icons.key_off_outlined, size: 18),
                ),
              IconButton(
                tooltip: '搜索本地资料（只查本机，不联网）',
                onPressed: () =>
                    setState(() => _localPanelOpen = !_localPanelOpen),
                icon: const Icon(Icons.manage_search, size: 20),
              ),
              IconButton(
                tooltip: '记忆',
                onPressed: _openMemory,
                icon: const Icon(Icons.memory, size: 20),
              ),
              IconButton(
                tooltip: '设置',
                onPressed: _openSettings,
                icon: const Icon(Icons.settings_outlined),
              ),
            ],
          ),
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
                  Text(
                    '本地资料（只查这台设备，不联网）',
                    style: theme.textTheme.bodySmall
                        ?.copyWith(fontWeight: FontWeight.w600),
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
              ? Center(
                  child: Text(
                    '问点什么吧——课表、成绩、校园生活都行',
                    style: theme.textTheme.bodySmall
                        ?.copyWith(color: theme.hintColor),
                  ),
                )
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

/// 一条消息气泡（中性：用主题语义色，不写死颜色）。
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
class _SettingsDialog extends StatefulWidget {
  const _SettingsDialog(
      {required this.settings,
      required this.searchLabel,
      required this.searchHint});

  final ChatSettings settings;
  final String searchLabel;
  final String searchHint;

  @override
  State<_SettingsDialog> createState() => _SettingsDialogState();
}

class _SettingsDialogState extends State<_SettingsDialog> {
  late final TextEditingController _key =
      TextEditingController(text: widget.settings.byokKey);
  late final TextEditingController _token =
      TextEditingController(text: widget.settings.cloudToken);
  late final TextEditingController _model =
      TextEditingController(text: widget.settings.model);
  late String _protocol = widget.settings.protocol;
  late String _channel = widget.settings.channel;

  @override
  void dispose() {
    _key.dispose();
    _token.dispose();
    _model.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return AlertDialog(
      // P0-2：任何主题都不透底——玻璃只用于外层装饰，表单必须实心可读
      backgroundColor: Theme.of(context).colorScheme.surface,
      surfaceTintColor: Colors.transparent,
      scrollable: true,
      insetPadding: const EdgeInsets.symmetric(horizontal: 16, vertical: 24),
      titlePadding: const EdgeInsets.fromLTRB(24, 20, 24, 8),
      contentPadding: const EdgeInsets.fromLTRB(24, 8, 24, 8),
      actionsPadding: const EdgeInsets.fromLTRB(24, 8, 24, 16),
      title: const Text('对话设置'),
      content: SingleChildScrollView(
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: <Widget>[
            // 联网搜索来源：**诚实展示**（未配置时把提示也放进设置页）
            Align(
              alignment: Alignment.centerLeft,
              child: Text('联网搜索：${widget.searchLabel}'),
            ),
            if (widget.searchHint.isNotEmpty)
              Padding(
                padding: const EdgeInsets.only(top: 4, bottom: 8),
                child: Text(
                  widget.searchHint,
                  style: Theme.of(context).textTheme.bodySmall,
                ),
              ),
            const SizedBox(height: 12),
            DropdownButtonFormField<String>(
              initialValue: _channel,
              decoration: const InputDecoration(labelText: '通道'),
              items: const <DropdownMenuItem<String>>[
                DropdownMenuItem<String>(value: 'byok', child: Text('BYOK 直连')),
                DropdownMenuItem<String>(value: 'cloud', child: Text('套餐通道')),
              ],
              onChanged: (String? value) =>
                  setState(() => _channel = value ?? _channel),
            ),
            const SizedBox(height: 12),
            TextField(
              controller: _model,
              decoration: const InputDecoration(labelText: '模型'),
            ),
            const SizedBox(height: 12),
            DropdownButtonFormField<String>(
              initialValue: _protocol,
              decoration: const InputDecoration(labelText: '协议'),
              items: const <DropdownMenuItem<String>>[
                DropdownMenuItem<String>(
                    value: 'openai_completions',
                    child: Text('OpenAI Completions')),
                DropdownMenuItem<String>(
                    value: 'openai_responses', child: Text('OpenAI Responses')),
                DropdownMenuItem<String>(
                    value: 'anthropic_messages',
                    child: Text('Anthropic Messages')),
              ],
              onChanged: (String? value) =>
                  setState(() => _protocol = value ?? _protocol),
            ),
            const SizedBox(height: 20),
            const Divider(height: 1),
            const SizedBox(height: 16),
            TextField(
              controller: _key,
              obscureText: true,
              enableSuggestions: false,
              autocorrect: false,
              decoration: const InputDecoration(
                labelText: 'BYOK API Key',
                hintText: 'sk-…',
                helperText: '只保存在本机；不会显示在界面上，也不进日志',
              ),
            ),
            const SizedBox(height: 12),
            TextField(
              controller: _token,
              obscureText: true,
              enableSuggestions: false,
              autocorrect: false,
              decoration: const InputDecoration(
                labelText: '套餐访问令牌',
                helperText: '套餐通道用；没有就留空',
              ),
            ),
          ],
        ),
      ),
      actions: <Widget>[
        TextButton(
            onPressed: () => Navigator.of(context).pop(),
            child: const Text('取消')),
        FilledButton(
          onPressed: () {
            widget.settings
              ..channel = _channel
              ..model = _model.text.trim().isEmpty
                  ? 'deepseek-chat'
                  : _model.text.trim()
              ..protocol = _protocol
              ..byokKey = _key.text.trim()
              ..cloudToken = _token.text.trim();
            Navigator.of(context).pop(widget.settings);
          },
          child: const Text('保存'),
        ),
      ],
    );
  }
}
