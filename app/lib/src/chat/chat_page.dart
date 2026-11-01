// 会话页（中性样式；视觉留待 designer 选型后换皮）。
//
// 结构刻意保持"可换皮"：只依赖 `Theme.of(context)` 的语义色，不写死配色/圆角；
// 消息列表与输入区拆成独立 widget，后续整体换主题不需要改逻辑。

import 'dart:async';

import 'package:flutter/material.dart';

import '../core/chat_service.dart';

/// 会话设置（MVP：密钥只在内存里，进程退出即失效）。
///
/// 下一步（t34 之后）：接 Rust keystore 的 `yunong_set/get_secret`，
/// 让 Key 落到系统安全存储、永不回显。本轮的 UI 已经按"不回显"设计。
class ChatSettings {
  String channel = 'byok';
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
  const ChatPage({super.key, this.service, this.settings});

  final ChatService? service;
  final ChatSettings? settings;

  @override
  State<ChatPage> createState() => _ChatPageState();
}

class _ChatPageState extends State<ChatPage> {
  late final ChatService _service = widget.service ?? ChatService();
  late final ChatSettings _settings = widget.settings ?? ChatSettings();
  final TextEditingController _input = TextEditingController();
  final ScrollController _scroll = ScrollController();

  final List<ChatBubble> _bubbles = <ChatBubble>[];
  ChatStream? _stream;
  bool _streaming = false;
  String? _error;
  int? _promptTokens;
  int? _completionTokens;
  String _channel = 'byok';

  @override
  void initState() {
    super.initState();
    _channel = _settings.channel;
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

  Future<void> _send() async {
    final text = _input.text.trim();
    if (text.isEmpty || _streaming) {
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
      apiKey: _settings.byokKey,
      cloudToken: _settings.cloudToken,
    );

    final stream = _service.send(request);
    _stream = stream;
    stream.events.listen(
      (ChatEvent event) {
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

  Future<void> _openSettings() async {
    final result = await showDialog<ChatSettings>(
      context: context,
      builder: (BuildContext context) => _SettingsDialog(settings: _settings),
    );
    if (result != null && mounted) {
      setState(() {
        _settings.byokKey = result.byokKey;
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
                },
              ),
              const SizedBox(width: 12),
              Expanded(
                child: Text(
                  _streaming ? '正在生成…' : '就绪',
                  style: theme.textTheme.bodySmall,
                ),
              ),
              if (_promptTokens != null || _completionTokens != null)
                Text(
                  '用量 ${_promptTokens ?? '-'} / ${_completionTokens ?? '-'}',
                  style: theme.textTheme.bodySmall,
                ),
              IconButton(
                tooltip: '设置',
                onPressed: _openSettings,
                icon: const Icon(Icons.settings_outlined),
              ),
            ],
          ),
        ),
        if (_error != null)
          Padding(
            padding: const EdgeInsets.fromLTRB(12, 8, 12, 0),
            child: Text(
              _error!,
              style: theme.textTheme.bodySmall?.copyWith(color: theme.colorScheme.error),
            ),
          ),
        Expanded(
          child: _bubbles.isEmpty
              ? Center(
                  child: Text(
                    '问点什么吧——课表、成绩、校园生活都行',
                    style: theme.textTheme.bodySmall?.copyWith(color: theme.hintColor),
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
        Padding(
          padding: const EdgeInsets.fromLTRB(12, 4, 12, 12),
          child: Row(
            children: <Widget>[
              Expanded(
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
          ),
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
    final theme = Theme.of(context);
    final isUser = bubble.fromUser;
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
          borderRadius: BorderRadius.circular(10),
        ),
        child: Text(
          bubble.text.isEmpty ? '…' : bubble.text,
          style: theme.textTheme.bodyMedium,
        ),
      ),
    );
  }
}

/// 设置对话框：Key 输入框 `obscureText`，**不回显**已保存的值。
class _SettingsDialog extends StatefulWidget {
  const _SettingsDialog({required this.settings});

  final ChatSettings settings;

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
      title: const Text('对话设置'),
      content: SingleChildScrollView(
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: <Widget>[
            DropdownButtonFormField<String>(
              initialValue: _channel,
              decoration: const InputDecoration(labelText: '通道'),
              items: const <DropdownMenuItem<String>>[
                DropdownMenuItem<String>(value: 'byok', child: Text('BYOK 直连')),
                DropdownMenuItem<String>(value: 'cloud', child: Text('套餐通道')),
              ],
              onChanged: (String? value) => setState(() => _channel = value ?? _channel),
            ),
            TextField(
              controller: _model,
              decoration: const InputDecoration(labelText: '模型'),
            ),
            DropdownButtonFormField<String>(
              initialValue: _protocol,
              decoration: const InputDecoration(labelText: '协议'),
              items: const <DropdownMenuItem<String>>[
                DropdownMenuItem<String>(
                    value: 'openai_completions', child: Text('OpenAI Completions')),
                DropdownMenuItem<String>(
                    value: 'openai_responses', child: Text('OpenAI Responses')),
                DropdownMenuItem<String>(
                    value: 'anthropic_messages', child: Text('Anthropic Messages')),
              ],
              onChanged: (String? value) => setState(() => _protocol = value ?? _protocol),
            ),
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
        TextButton(onPressed: () => Navigator.of(context).pop(), child: const Text('取消')),
        FilledButton(
          onPressed: () {
            widget.settings
              ..channel = _channel
              ..model = _model.text.trim().isEmpty ? 'deepseek-chat' : _model.text.trim()
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
