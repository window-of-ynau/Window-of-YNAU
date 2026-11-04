// 对话设置（t85：从 `chat_page.dart` **纯搬迁**，行为不变）。
//
// 搬迁目的：下一步「合并两个设置面板」时，设置页可以直接引用本文件的
// [ChatSettingsSection]，对话页仅保留一个入口——届时改动面只在引用关系上。
//
// 注意：本文件**不** import `chat_page.dart`（否则循环依赖）；
// [ChatSettings] 数据类也随设置面板一起搬到这里，`chat_page.dart` 反向 import。

import 'package:flutter/material.dart';

import '../core/auth_service.dart';
import '../core/secret_service.dart';
import '../core/provider_store.dart';
/// 对话设置（通道/模型/协议/Key/令牌）——随设置面板一起搬迁。
class ChatSettings {
  String channel = 'byok';

  /// 上游提供方（架构预留：加 OpenAI / 自建网关只需加枚举值）。
  String providerId = 'deepseek';

  /// 该提供方的 OpenAI 兼容 base url（BYOK 直连用）。
  String baseUrl = 'https://api.deepseek.com';
  String byokKey = '';
  String cloudToken = '';
  /// 默认模型：文档值为 `deepseek-flash`（见 provider_store.dart 顶部来源说明）。
  String model = DeepSeekPreset.defaultModel;
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
    this.secrets,
    this.keyValidator,
    this.auth,
    this.onKeySaved,
  });

  final ChatSettings settings;
  final String searchLabel;
  final String searchHint;
  final String accountLabel;
  final bool signedIn;
  final Future<void> Function()? onSignIn;
  final Future<void> Function()? onSignOut;

  /// 下沉的 Key 能力（t88）：给了 `secrets` 才能在本面板内完成“校验→保存→回读”。
  final SecretService? secrets;

  /// 保存前的 Key 校验（与对话页同一套规则）。
  final Future<String?> Function(String apiKey)? keyValidator;

  /// 下沉的账号能力（t88）：给了 `auth` 才能在本面板内完成套餐登录/退出。
  final AuthService? auth;

  /// 保存成功回调（对话页用它刷新 `_keyStatus`）。
  final void Function(SecretStatus status)? onKeySaved;

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
          onPressed: () async {
            widget.settings
              ..channel = _channel
              ..model = _model.text.trim().isEmpty
                  ? DeepSeekPreset.defaultModel
                  : _model.text.trim()
              ..protocol = _protocol
              ..byokKey = _key.text.trim()
              ..cloudToken = _token.text.trim();
            // t88：Key 能力已下沉——给了 secrets 就在面板内完成“校验 → 保存 → 回读”
            final apiKey = _key.text.trim();
            final secrets = widget.secrets;
            if (secrets != null && apiKey.isNotEmpty) {
              try {
                final status = await saveKeyWithService(
                  secrets: secrets,
                  apiKey: apiKey,
                  validate: widget.keyValidator,
                );
                widget.onKeySaved?.call(status);
              } on ChatKeySaveException catch (failure) {
                if (context.mounted) {
                  ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text(failure.message)));
                }
                return; // 保存失败不关闭，让用户改
              }
            }
            if (context.mounted) {
              Navigator.of(context).pop(widget.settings);
            }
          },
          child: const Text('保存'),
        ),
      ],
    );
  }
}

/// 对话设置的**共享状态源**（t86）：设置页与对话页用同一个实例，
/// 避免“设置页改了、对话页没变”。
class ChatSettingsController extends ChangeNotifier {
  ChatSettingsController({ChatSettings? initial}) : settings = initial ?? ChatSettings();

  final ChatSettings settings;

  /// 摘要（设置页卡片用）：通道 / 模型 / 协议。
  String get summary {
    final String channelLabel = settings.channel == 'cloud' ? '套餐通道' : 'BYOK 直连';
    return '通道：$channelLabel · 模型：${settings.model} · 协议：${protocolLabel(settings.protocol)}';
  }

  static String protocolLabel(String protocol) {
    switch (protocol) {
      case 'openai_responses':
        return 'OpenAI Responses';
      case 'anthropic_messages':
        return 'Anthropic Messages';
      default:
        return 'OpenAI Completions';
    }
  }

  /// 保存后合并新值并通知两处刷新。
  void apply(ChatSettings next) {
    settings
      ..channel = next.channel
      ..providerId = next.providerId
      ..baseUrl = next.baseUrl
      ..byokKey = next.byokKey
      ..cloudToken = next.cloudToken
      ..model = next.model
      ..protocol = next.protocol;
    notifyListeners();
  }
}

/// 套餐账号登录对话框（邮箱 + 密码；密码不回显）。**公开**：设置页与对话页共用。
class CloudLoginDialog extends StatefulWidget {
  const CloudLoginDialog({super.key});

  @override
  State<CloudLoginDialog> createState() => CloudLoginDialogState();
}

class CloudLoginDialogState extends State<CloudLoginDialog> {
  final TextEditingController _email = TextEditingController();
  final TextEditingController _password = TextEditingController();

  @override
  void dispose() {
    _email.dispose();
    _password.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return AlertDialog(
      backgroundColor: Theme.of(context).colorScheme.surface,
      surfaceTintColor: Colors.transparent,
      scrollable: true,
      title: const Text('登录套餐账号'),
      content: Column(
        mainAxisSize: MainAxisSize.min,
        children: <Widget>[
          TextField(
            controller: _email,
            keyboardType: TextInputType.emailAddress,
            decoration: const InputDecoration(labelText: '邮箱'),
          ),
          const SizedBox(height: 12),
          TextField(
            controller: _password,
            obscureText: true,
            enableSuggestions: false,
            autocorrect: false,
            decoration: const InputDecoration(labelText: '密码'),
          ),
        ],
      ),
      actions: <Widget>[
        TextButton(onPressed: () => Navigator.of(context).pop(), child: const Text('取消')),
        FilledButton(
          onPressed: () {
            final email = _email.text.trim();
            final password = _password.text;
            if (email.isEmpty || password.isEmpty) {
              return;
            }
            Navigator.of(context).pop((email: email, password: password));
          },
          child: const Text('登录'),
        ),
      ],
    );
  }
}

/// 保存 Key 的校验/回读逻辑（t88 从 `ChatPage` 下沉）：可在面板内独立测试。
Future<SecretStatus> saveKeyWithService({
  required SecretService secrets,
  required String apiKey,
  Future<String?> Function(String apiKey)? validate,
}) async {
  final String? reason = validate == null ? null : await validate(apiKey);
  if (reason != null) {
    throw ChatKeySaveException(reason);
  }
  final SecretStatus saved = await secrets.save(SecretService.byokKeyName, apiKey);
  if (!saved.set) {
    throw const ChatKeySaveException('保存失败：安全存储没有确认写入');
  }
  final SecretStatus status = await secrets.status(SecretService.byokKeyName);
  if (!status.set || status.masked.isEmpty) {
    throw const ChatKeySaveException('保存后回读失败：Key 没有真正落盘');
  }
  return status;
}

/// 保存 Key 失败（消息可直接展示给用户）。
class ChatKeySaveException implements Exception {
  const ChatKeySaveException(this.message);

  final String message;
}
