// 【临时诊断】会话校验探针的入口弹窗 —— 验证后删除。
//
// 用法：设置页 →「关于」→「[临时] 会话校验探针」，点「运行」。
// 会从 WebView 的 Cookie 管理器取当前校务会话 Cookie，再用纯 HTTP（dart:io）打一次 GET。
// Cookie 只在内存里用于拼请求头，**不打印、不上传、不写文件**。

import 'package:flutter/material.dart';
import 'package:flutter_inappwebview/flutter_inappwebview.dart';

import 'campus_entries.dart';
import 'session_probe.dart';

/// 读取校务域当前会话 Cookie（可能为空：还没登录过）。
Future<List<ProbeCookie>> loadCampusCookies() async {
  final List<ProbeCookie> cookies = <ProbeCookie>[];
  // 门户与统一身份认证两个域都取一遍（登录态通常落在其一）。
  for (final String url in <String>[
    CampusEntry.portal.url,
    'https://ssoo.ynau.edu.cn/',
    'https://jwxt.ynau.edu.cn/',
  ]) {
    try {
      final List<Cookie> found = await CookieManager.instance().getCookies(url: WebUri(url));
      for (final Cookie cookie in found) {
        if (cookies.any((ProbeCookie item) => item.name == '${cookie.name}')) {
          continue;
        }
        cookies.add(ProbeCookie('${cookie.name}', '${cookie.value}'));
      }
    } on Object {
      // 没有 WebView 会话/平台不支持：当作没有 Cookie，UI 会提示先登录。
    }
  }
  return cookies;
}

class SessionProbeDialog extends StatefulWidget {
  const SessionProbeDialog({super.key, this.cookieLoader, this.client});

  /// 测试注入；默认走真实 CookieManager。
  final Future<List<ProbeCookie>> Function()? cookieLoader;
  final ProbeClient? client;

  @override
  State<SessionProbeDialog> createState() => _SessionProbeDialogState();
}

class _SessionProbeDialogState extends State<SessionProbeDialog> {
  final TextEditingController _url = TextEditingController(text: kProbeDefaultUrl);
  final TextEditingController _ua = TextEditingController(text: kProbeDefaultUserAgent);
  SessionProbeResult? _result;
  List<ProbeCookie> _cookies = const <ProbeCookie>[];
  bool _busy = false;
  String? _notice;

  @override
  void initState() {
    super.initState();
    _loadCookies();
  }

  @override
  void dispose() {
    _url.dispose();
    _ua.dispose();
    super.dispose();
  }

  Future<void> _loadCookies() async {
    setState(() => _busy = true);
    try {
      final List<ProbeCookie> cookies =
          await (widget.cookieLoader ?? loadCampusCookies)();
      if (!mounted) {
        return;
      }
      setState(() {
        _cookies = cookies;
        _notice = cookies.isEmpty ? '没有取到校务会话 Cookie：请先在「我的课表」里登录一次校务系统。' : null;
      });
    } finally {
      if (mounted) {
        setState(() => _busy = false);
      }
    }
  }

  Future<void> _run() async {
    setState(() {
      _busy = true;
      _result = null;
    });
    final SessionProbeResult result = await runSessionProbe(
      url: _url.text,
      userAgent: _ua.text,
      cookies: _cookies,
      client: widget.client,
    );
    if (!mounted) {
      return;
    }
    setState(() {
      _result = result;
      _busy = false;
    });
  }

  @override
  Widget build(BuildContext context) {
    final ThemeData theme = Theme.of(context);
    final SessionProbeResult? result = _result;
    return AlertDialog(
      title: const Text('[临时] 会话校验探针'),
      content: SizedBox(
        width: 560,
        child: SingleChildScrollView(
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.start,
            children: <Widget>[
              Text(
                '临时诊断，验证后删除。Cookie 只在内存里用于拼请求头：不打印、不上传、不写文件。',
                style: theme.textTheme.bodySmall?.copyWith(color: theme.hintColor),
              ),
              const SizedBox(height: 8),
              TextField(
                key: const Key('probe-url'),
                controller: _url,
                decoration: const InputDecoration(labelText: '目标 URL（只发 GET）'),
              ),
              TextField(
                key: const Key('probe-ua'),
                controller: _ua,
                decoration: const InputDecoration(labelText: 'User-Agent（换一个再试，可判断有无 UA 校验）'),
              ),
              const SizedBox(height: 8),
              if (_notice != null) Text(_notice!, style: theme.textTheme.bodySmall),
              Wrap(
                spacing: 8,
                children: <Widget>[
                  Text('Cookie：${_cookies.length} 条（不含内容）', style: theme.textTheme.bodySmall),
                  TextButton(onPressed: _busy ? null : _loadCookies, child: const Text('重新读取会话')),
                ],
              ),
              if (result != null) ...<Widget>[
                const Divider(),
                Text(result.verdict, style: theme.textTheme.titleSmall),
                const SizedBox(height: 6),
                _kv('状态码', '${result.statusCode}'),
                _kv('Content-Type', result.contentType.isEmpty ? '—' : result.contentType),
                _kv('响应体长度', '${result.bodyLength}'),
                _kv('命中 10~12 位数字', result.studentIdHit ? '是（长度 ${result.studentIdMatchLength}）' : '否'),
                _kv('前 200 字哈希', result.bodyHash.isEmpty ? '—' : result.bodyHash),
                _kv('耗时', '${result.elapsedMs} ms'),
                _kv('UA', result.userAgent),
                if (result.error != null) _kv('错误', result.error!),
              ],
            ],
          ),
        ),
      ),
      actions: <Widget>[
        TextButton(onPressed: () => Navigator.pop(context), child: const Text('关闭')),
        FilledButton(
          key: const Key('probe-run'),
          onPressed: _busy ? null : _run,
          child: Text(result == null ? '运行' : '重试'),
        ),
      ],
    );
  }

  Widget _kv(String key, String value) => Padding(
        padding: const EdgeInsets.symmetric(vertical: 2),
        child: Row(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: <Widget>[
            SizedBox(width: 130, child: Text(key)),
            Expanded(child: SelectableText(value)),
          ],
        ),
      );
}
