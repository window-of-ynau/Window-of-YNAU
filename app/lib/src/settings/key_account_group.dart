// 设置页「模型与凭据」：Key 的存储状态 + 删除，以及套餐账号登录/退出（t112）。
//
// 为什么单独一个组件：
// - 「对话设置」只负责通道/模型/协议/Key **输入**；
// - 这里只放**状态与账号动作**，避免两处重复同一个字段（owner 提的第三项）。

import 'package:flutter/material.dart';

import '../core/auth_service.dart';
import '../core/secret_service.dart';

class KeyAccountGroup extends StatefulWidget {
  const KeyAccountGroup({super.key, required this.auth, required this.secrets});

  final AuthService? auth;
  final SecretService? secrets;

  @override
  State<KeyAccountGroup> createState() => _KeyAccountGroupState();
}

class _KeyAccountGroupState extends State<KeyAccountGroup> {
  SecretStatus? _status;
  String? _keyError;
  bool _busy = false;

  @override
  void initState() {
    super.initState();
    _refreshKey();
  }

  Future<void> _refreshKey() async {
    final SecretService? secrets = widget.secrets;
    if (secrets == null) {
      return;
    }
    try {
      final SecretStatus status = await secrets.status(SecretService.byokKeyName);
      if (!mounted) {
        return;
      }
      setState(() {
        _status = status;
        _keyError = null;
      });
    } on Object catch (failure) {
      if (!mounted) {
        return;
      }
      // 读不到状态时**不清空既有显示**，也不谎报“未设置”。
      setState(() => _keyError = '无法读取本机密钥状态：$failure');
    }
  }

  Future<void> _deleteKey() async {
    final SecretService? secrets = widget.secrets;
    if (secrets == null) {
      return;
    }
    final bool? confirmed = await showDialog<bool>(
      context: context,
      builder: (BuildContext context) => AlertDialog(
        title: const Text('删除已保存的 Key？'),
        content: const Text('删除后直连通道需要重新填写 Key；套餐账号不受影响。'),
        actions: <Widget>[
          TextButton(onPressed: () => Navigator.pop(context, false), child: const Text('取消')),
          FilledButton(onPressed: () => Navigator.pop(context, true), child: const Text('删除')),
        ],
      ),
    );
    if (confirmed != true) {
      return;
    }
    setState(() => _busy = true);
    try {
      await secrets.delete(SecretService.byokKeyName);
      await _refreshKey();
    } on Object catch (failure) {
      if (mounted) {
        ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text('删除失败：$failure')));
      }
    } finally {
      if (mounted) {
        setState(() => _busy = false);
      }
    }
  }

  Future<void> _signIn() async {
    final AuthService? auth = widget.auth;
    if (auth == null) {
      return;
    }
    final ({String email, String password})? result =
        await showDialog<({String email, String password})>(
      context: context,
      builder: (BuildContext context) => const _AccountDialog(),
    );
    if (result == null || !mounted) {
      return;
    }
    setState(() => _busy = true);
    try {
      await auth.signIn(email: result.email, password: result.password);
      if (mounted) {
        setState(() {});
        ScaffoldMessenger.of(context).showSnackBar(
          SnackBar(content: Text('已登录套餐账号 ${auth.maskedEmail}')),
        );
      }
    } on AuthException catch (failure) {
      if (mounted) {
        ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text(failure.userHint)));
      }
    } on Object {
      if (mounted) {
        ScaffoldMessenger.of(context).showSnackBar(const SnackBar(content: Text('登录失败，请稍后重试')));
      }
    } finally {
      if (mounted) {
        setState(() => _busy = false);
      }
    }
  }

  Future<void> _signOut() async {
    final AuthService? auth = widget.auth;
    if (auth == null) {
      return;
    }
    setState(() => _busy = true);
    try {
      await auth.signOut();
      if (mounted) {
        setState(() {});
      }
    } on Object {
      if (mounted) {
        ScaffoldMessenger.of(context).showSnackBar(const SnackBar(content: Text('退出失败，请稍后重试')));
      }
    } finally {
      if (mounted) {
        setState(() => _busy = false);
      }
    }
  }

  @override
  Widget build(BuildContext context) {
    final ThemeData theme = Theme.of(context);
    final AuthService? auth = widget.auth;
    final SecretService? secrets = widget.secrets;
    final bool available = auth != null || secrets != null;

    final String keySubtitle = _keyError ??
        (secrets == null
            ? '端侧核心不可用，暂无法读取密钥状态'
            : (_status == null ? '正在读取…' : '本机安全存储：${_status!.display}${_status!.broken ? '（密文损坏，请重新填写）' : ''}'));

    // 外层 _Group 是带底色的 DecoratedBox：包一层透明 Material，ListTile 的水墨才可见
    return Material(
      type: MaterialType.transparency,
      child: Column(
      children: <Widget>[
        ListTile(
          leading: const Icon(Icons.key_outlined),
          title: const Text('BYOK 密钥'),
          subtitle: Text(keySubtitle),
          trailing: (_status?.set ?? false)
              ? TextButton(
                  onPressed: _busy ? null : () => _deleteKey(),
                  child: const Text('删除'),
                )
              : null,
        ),
        ListTile(
          leading: const Icon(Icons.account_circle_outlined),
          title: const Text('套餐账号'),
          subtitle: Text(auth == null
              ? '端侧核心不可用，暂无法读取账号状态'
              : (auth.signedIn ? auth.maskedEmail : '未登录')),
          trailing: auth == null
              ? null
              : TextButton(
                  onPressed: _busy ? null : (auth.signedIn ? _signOut : _signIn),
                  child: Text(auth.signedIn ? '退出' : '登录'),
                ),
        ),
        if (!available)
          Padding(
            padding: const EdgeInsets.fromLTRB(16, 0, 16, 12),
            child: Text(
              '密钥与账号需要在包含端侧核心的构建里配置。',
              style: theme.textTheme.bodySmall?.copyWith(color: theme.hintColor),
            ),
          ),
      ],
      ),
    );
  }
}

/// 套餐账号登录对话框（邮箱 + 密码；密码不回显）。
class _AccountDialog extends StatefulWidget {
  const _AccountDialog();

  @override
  State<_AccountDialog> createState() => _AccountDialogState();
}

class _AccountDialogState extends State<_AccountDialog> {
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
      title: const Text('登录套餐账号'),
      content: Column(
        mainAxisSize: MainAxisSize.min,
        children: <Widget>[
          TextField(
            controller: _email,
            autofocus: true,
            decoration: const InputDecoration(labelText: '邮箱'),
          ),
          TextField(
            controller: _password,
            obscureText: true,
            decoration: const InputDecoration(labelText: '密码'),
          ),
        ],
      ),
      actions: <Widget>[
        TextButton(onPressed: () => Navigator.pop(context), child: const Text('取消')),
        FilledButton(
          onPressed: () {
            final String email = _email.text.trim();
            final String password = _password.text;
            if (email.isEmpty || password.isEmpty) {
              return;
            }
            Navigator.pop(context, (email: email, password: password));
          },
          child: const Text('登录'),
        ),
      ],
    );
  }
}
