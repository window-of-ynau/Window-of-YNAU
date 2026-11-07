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
  // t115：Key 状态显示已移到「供应商」卡；这里只保留套餐账号。

  bool _busy = false;

  @override
  void initState() {
    super.initState();
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


    // 外层 _Group 是带底色的 DecoratedBox：包一层透明 Material，ListTile 的水墨才可见
    return Material(
      type: MaterialType.transparency,
      child: Column(
      children: <Widget>[
        // t115：Key 输入只在「供应商」卡里（这里不再出现第二个可填 Key 的位置）
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
