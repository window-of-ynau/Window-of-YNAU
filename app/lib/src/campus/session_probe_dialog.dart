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
import 'webview_session.dart';

/// 读取校务域当前会话 Cookie（可能为空：还没登录过）。
///
/// t138 关键修复：**必须传 `webViewEnvironment`**。
/// 插件 Windows 实现（`flutter_inappwebview_windows-0.6.0/windows/cookie_manager.cpp:26-29`）
/// 在缺省环境下把 `webViewEnvironment` 解析成 `nullptr`，随后 `getCookies()`（同文件 :211/:224）
/// 调用 `webViewEnvironment->getWebView()` 失败 → **静默返回空列表**。
/// 这就是「Cookie 恒为 0」的根因，与 HttpOnly 无关（CDP `Network.getCookies` 本身能返回 HttpOnly Cookie）。
Future<List<ProbeCookie>> loadCampusCookies() async {
  final List<ProbeCookie> cookies = <ProbeCookie>[];
  // t138：带上我们自己的 WebViewEnvironment，否则插件静默返回空列表（见下方注释）。
  final WebViewEnvironment? environment = await WebViewSession.ensureEnvironment();
  // 门户与统一身份认证两个域都取一遍（登录态通常落在其一）。
  for (final String url in <String>[
    CampusEntry.portal.url,
    'https://ssoo.ynau.edu.cn/',
    'https://jwxt.ynau.edu.cn/',
    'https://one.ynau.edu.cn/',
  ]) {
    try {
      final List<Cookie> found = await CookieManager.instance(webViewEnvironment: environment)
          .getCookies(url: WebUri(url));
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
  final WebViewEnvironment? environment = await WebViewSession.ensureEnvironment();
  final List<Cookie> found = await CookieManager.instance(webViewEnvironment: environment)
      .getCookies(url: WebUri(url.toString()));
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
    this.navigate,
  });

  /// 测试注入；默认走真实 CookieManager。
  final Future<List<ProbeCookie>> Function()? cookieLoader;
  final ProbeClient? client;

  /// t137：三步诊断的注入点（默认走 CookieManager + WebView JS 桥）。
  final CookieRefSource? cookieRefSource;
  final JsCookieLengthProbe? jsProbe;

  /// t138：CAS 探针的导航注入点（默认用课表页的 WebView）。
  final Future<void> Function(String url)? navigate;

  @override
  State<SessionProbeDialog> createState() => _SessionProbeDialogState();
}

class _SessionProbeDialogState extends State<SessionProbeDialog> {
  final TextEditingController _url = TextEditingController(text: kProbeDefaultUrl);
  final TextEditingController _ua = TextEditingController(text: kProbeDefaultUserAgent);
  final TextEditingController _casBase = TextEditingController(text: CasTicketProbe.defaultCasBase);
  final TextEditingController _serviceUrl =
      TextEditingController(text: CasTicketProbe.defaultServiceUrl);
  SessionProbeResult? _result;
  CookieDiagnostics? _diagnostics;
  List<HeaderTrial> _headers = const <HeaderTrial>[];
  CasTicketResult? _cas;
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
    _casBase.dispose();
    _serviceUrl.dispose();
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

  /// t138：三组请求头对照（裸 / 加浏览器头 / 加浏览器头+同 UA）。
  Future<void> _runHeaderMatrix() async {
    setState(() => _busy = true);
    final List<HeaderTrial> trials = await runHeaderMatrix(
      url: _url.text,
      userAgent: _ua.text,
      cookies: _cookies,
      client: widget.client,
    );
    if (!mounted) {
      return;
    }
    setState(() {
      _headers = trials;
      _busy = false;
    });
  }

  /// t138：CAS ticket 探针（方案 E）——导航 + 记录重定向链，只看是否有 `ticket=ST-`。
  Future<void> _runCasProbe() async {
    setState(() {
      _busy = true;
      _cas = null;
    });
    final String loginUrl = CasTicketProbe.loginUrl(
      casBase: _casBase.text,
      serviceUrl: _serviceUrl.text,
    );
    final Future<void> Function(String url)? navigate = widget.navigate ?? CasTicketProbe.navigate;
    if (navigate == null) {
      setState(() {
        _busy = false;
        _notice = '当前没有可用的 WebView：请先打开「我的课表」再回来试 CAS 探针。';
      });
      return;
    }
    CasTicketProbe.start();
    try {
      await navigate(loginUrl);
      // 给重定向链一点时间收敛（CAS 通常是 2~4 跳）
      await Future<void>.delayed(const Duration(seconds: 6));
    } finally {
      CasTicketProbe.stop();
    }
    if (!mounted) {
      return;
    }
    setState(() {
      _cas = CasTicketResult.summarize(List<String>.of(CasTicketProbe.redirects));
      _busy = false;
    });
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
              // t138：把「结论区」顶到最上面（owner 反馈：诊断输出藏在下面看不到）
              if (_diagnostics != null || _headers.isNotEmpty || _cas != null)
                Container(
                  key: const Key('probe-verdict-box'),
                  width: double.infinity,
                  margin: const EdgeInsets.only(bottom: 8),
                  padding: const EdgeInsets.all(10),
                  decoration: BoxDecoration(
                    color: theme.colorScheme.errorContainer.withValues(alpha: 0.35),
                    borderRadius: BorderRadius.circular(8),
                  ),
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: <Widget>[
                      if (_diagnostics != null) ...<Widget>[
                        Text('Cookie 诊断：${_diagnostics!.diagnosis}', style: theme.textTheme.titleSmall),
                        Text(
                          'CookieManager 精确 URL：${_diagnostics!.exact.length} 条 · '
                          'JS document.cookie 长度：${_diagnostics!.jsCookieLength ?? '拿不到'}',
                          style: theme.textTheme.bodySmall,
                        ),
                      ],
                      if (_headers.isNotEmpty) ...<Widget>[
                        const SizedBox(height: 6),
                        Text('请求头对照：${headerMatrixVerdict(_headers)}',
                            style: theme.textTheme.titleSmall),
                      ],
                      if (_cas != null) ...<Widget>[
                        const SizedBox(height: 6),
                        Text('CAS ticket：${_cas!.verdict}', style: theme.textTheme.titleSmall),
                      ],
                    ],
                  ),
                ),
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
              // t138：三组请求头对照（裸 / 加浏览器头 / 加浏览器头+同 UA）
              Wrap(
                spacing: 8,
                crossAxisAlignment: WrapCrossAlignment.center,
                children: <Widget>[
                  OutlinedButton.icon(
                    key: const Key('probe-headers'),
                    onPressed: _busy ? null : _runHeaderMatrix,
                    icon: const Icon(Icons.travel_explore, size: 18),
                    label: const Text('运行请求头对照（3 组）'),
                  ),
                  if (_headers.isNotEmpty)
                    Text(
                      _headers
                          .map((HeaderTrial t) => '${t.label}=${t.run.statusCode}')
                          .join(' · '),
                      style: theme.textTheme.bodySmall,
                    ),
                ],
              ),
              if (_headers.isNotEmpty) ...<Widget>[
                for (final HeaderTrial trial in _headers)
                  Text(
                    '${trial.label}：${trial.run.statusCode} · ${trial.run.bodyLength} 字节 · '
                    '${trial.run.elapsedMs} ms · 哈希 ${trial.run.bodyHash.isEmpty ? '—' : trial.run.bodyHash}',
                    style: theme.textTheme.bodySmall,
                  ),
              ],
              const SizedBox(height: 8),
              // t138：CAS service ticket 探针（方案 E）
              Text('CAS ticket 探针（方案 E）', style: theme.textTheme.titleSmall),
              TextField(
                key: const Key('probe-cas-base'),
                controller: _casBase,
                decoration: const InputDecoration(labelText: 'CAS 基址'),
              ),
              TextField(
                key: const Key('probe-service'),
                controller: _serviceUrl,
                decoration: const InputDecoration(labelText: 'service 地址（登录成功后会跳回它并带 ticket）'),
              ),
              const SizedBox(height: 4),
              OutlinedButton.icon(
                key: const Key('probe-cas'),
                onPressed: _busy ? null : _runCasProbe,
                icon: const Icon(Icons.vpn_key_outlined, size: 18),
                label: const Text('测试 CAS ticket'),
              ),
              if (_cas != null) ...<Widget>[
                Text(
                  '是否拿到 ticket：${_cas!.ticketFound ? '是（ST-${_cas!.prefix}****）' : '否'} · '
                  '跳转 ${_cas!.hops} 次 · 终点域 ${_cas!.finalHost.isEmpty ? '—' : _cas!.finalHost}',
                  style: theme.textTheme.bodySmall,
                ),
                for (final String hop in _cas!.redirects.take(12))
                  Text('· ${shortUrl(hop)}', style: theme.textTheme.bodySmall),
              ],
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
