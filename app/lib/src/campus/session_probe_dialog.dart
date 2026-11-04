// 【临时诊断】会话校验探针的入口弹窗 —— 验证后删除。
//
// 用法：设置页 →「关于」→「[临时] 会话校验探针」，点「运行」。
// 会从 WebView 的 Cookie 管理器取当前校务会话 Cookie，再用纯 HTTP（dart:io）打一次 GET。
// Cookie 只在内存里用于拼请求头，**不打印、不上传、不写文件**。

import 'package:flutter/material.dart';
import 'package:flutter_inappwebview/flutter_inappwebview.dart';

import 'campus_entries.dart';
import 'session_probe.dart';
import 'session_cookie_diagnostics.dart';

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

/// 【临时诊断 t137】按**精确 URL** 取 Cookie 元信息（只有名字/域，**没有值**）。
Future<List<CookieRef>> loadCampusCookieRefs(Uri url) async {
  final List<Cookie> found = await CookieManager.instance().getCookies(url: WebUri(url.toString()));
  return found
      .map((Cookie cookie) => CookieRef(name: '${cookie.name}', domain: '${cookie.domain ?? ''}'))
      .toList(growable: false);
}

class SessionProbeDialog extends StatefulWidget {
  const SessionProbeDialog({
    super.key,
    this.cookieLoader,
    this.client,
    this.cookieRefSource,
    this.jsProbe,
  });

  /// 测试注入；默认走真实 CookieManager。
  final Future<List<ProbeCookie>> Function()? cookieLoader;
  final ProbeClient? client;

  /// t137：三步诊断的注入点（默认走 CookieManager + WebView JS 桥）。
  final CookieRefSource? cookieRefSource;
  final JsCookieLengthProbe? jsProbe;

  @override
  State<SessionProbeDialog> createState() => _SessionProbeDialogState();
}

class _SessionProbeDialogState extends State<SessionProbeDialog> {
  final TextEditingController _url = TextEditingController(text: kProbeDefaultUrl);
  final TextEditingController _ua = TextEditingController(text: kProbeDefaultUserAgent);
  SessionProbeResult? _result;
  CookieDiagnostics? _diagnostics;
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

  /// t137：三步诊断（精确 URL / JS 对照 / 跨域扫描）。
  Future<void> _runDiagnostics() async {
    setState(() => _busy = true);
    final CookieDiagnostics diagnostics = await diagnoseCookies(
      target: Uri.parse(_url.text),
      source: widget.cookieRefSource ?? loadCampusCookieRefs,
      jsProbe: widget.jsProbe ?? SessionProbeJsBridge.documentCookieLength,
    );
    if (!mounted) {
      return;
    }
    setState(() {
      _diagnostics = diagnostics;
      _busy = false;
    });
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
              // t137：三步诊断（精确 URL / JS 对照 / 跨域扫描）
              Wrap(
                spacing: 8,
                crossAxisAlignment: WrapCrossAlignment.center,
                children: <Widget>[
                  OutlinedButton.icon(
                    key: const Key('probe-diagnose'),
                    onPressed: _busy ? null : _runDiagnostics,
                    icon: const Icon(Icons.bug_report_outlined, size: 18),
                    label: const Text('运行三步诊断'),
                  ),
                  if (_diagnostics != null)
                    Text(
                      'JS document.cookie 长度：${_diagnostics!.jsCookieLength ?? '拿不到'}',
                      style: theme.textTheme.bodySmall,
                    ),
                ],
              ),
              if (_diagnostics != null) ...<Widget>[
                const SizedBox(height: 6),
                Text(_diagnostics!.diagnosis, style: theme.textTheme.titleSmall),
                const SizedBox(height: 4),
                Text(
                  '精确 URL（${_diagnostics!.targetUrl}）可见 Cookie：${_diagnostics!.exact.length} 条'
                  '${_diagnostics!.exactError == null ? '' : '（查询失败：${_diagnostics!.exactError}）'}',
                  style: theme.textTheme.bodySmall,
                ),
                if (_diagnostics!.exact.isNotEmpty)
                  Text(describeCookieRefs(_diagnostics!.exact), style: theme.textTheme.bodySmall),
                const SizedBox(height: 4),
                for (final DomainScan scan in _diagnostics!.scans)
                  Text(
                    '${scan.domain}：${scan.count} 条'
                    '${scan.cookies.isEmpty ? '' : '（${scan.cookies.map((CookieRef c) => c.name).join(', ')}）'}',
                    style: theme.textTheme.bodySmall,
                  ),
                const Divider(),
              ],
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
                _kv('会话 Cookie', '${result.cookieCount} 条（不含内容）'),
                _kv('两次是否同页', result.sameAsAnonymous ? '是（公开页面/登录页 → 结果无效）' : '否'),
                _runCard('带 Cookie', result.withCookie),
                _runCard('匿名对照（不带 Cookie）', result.anonymous),
                _kv('UA', result.userAgent),
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

  Widget _runCard(String label, ProbeRun run) => Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: <Widget>[
          Padding(
            padding: const EdgeInsets.only(top: 10, bottom: 2),
            child: Text(label, style: const TextStyle(fontWeight: FontWeight.w600)),
          ),
          _kv('状态码', '${run.statusCode}'),
          _kv('Content-Type', run.contentType.isEmpty ? '—' : run.contentType),
          _kv('响应体长度', '${run.bodyLength}'),
          _kv('学号片段', run.studentIdMasked ?? '未命中'),
          _kv('页面标题', run.title.isEmpty ? '—' : run.title),
          _kv('含「登录」字样', run.loginHint ? '是（多半是登录页）' : '否'),
          _kv('前 200 字哈希', run.bodyHash.isEmpty ? '—' : run.bodyHash),
          _kv('耗时', '${run.elapsedMs} ms'),
          if (run.error != null) _kv('错误', run.error!),
        ],
      );

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
