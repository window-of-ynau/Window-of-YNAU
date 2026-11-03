// 对话设置（t85：从 `chat_page.dart` **纯搬迁**，行为不变）。
//
// 搬迁目的：下一步「合并两个设置面板」时，设置页可以直接引用本文件的
// [ChatSettingsSection]，对话页仅保留一个入口——届时改动面只在引用关系上。
//
// 注意：本文件**不** import `chat_page.dart`（否则循环依赖）；
// [ChatSettings] 数据类也随设置面板一起搬到这里，`chat_page.dart` 反向 import。

import 'package:flutter/material.dart';
/// 对话设置（通道/模型/协议/Key/令牌）——随设置面板一起搬迁。
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


/// 对话设置面板（原 `_SettingsDialog`，纯搬迁并公开；构造参数与行为完全不变）。
class ChatSettingsSection extends StatefulWidget {
  const ChatSettingsSection({
    super.key,
    required this.settings,
    required this.searchLabel,
    required this.searchHint,
    this.accountLabel = '',
    this.signedIn = false,
    this.onSignIn,
    this.onSignOut,
  });

  final ChatSettings settings;
  final String searchLabel;
  final String searchHint;
  final String accountLabel;
  final bool signedIn;
  final Future<void> Function()? onSignIn;
  final Future<void> Function()? onSignOut;

  @override
  State<ChatSettingsSection> createState() => _ChatSettingsSectionState();
}

class _ChatSettingsSectionState extends State<ChatSettingsSection> {
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
            const SizedBox(height: 8),
            Row(
              children: <Widget>[
                Expanded(child: Text('套餐账号：${widget.accountLabel}')),
                TextButton(
                  onPressed: () async {
                    if (widget.signedIn) {
                      await widget.onSignOut?.call();
                    } else {
                      await widget.onSignIn?.call();
                    }
                  },
                  child: Text(widget.signedIn ? '退出' : '登录'),
                ),
              ],
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
                helperText: '填到域名即可，带不带 /v1 都行；只保存在本机，不回显、不进日志',
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
