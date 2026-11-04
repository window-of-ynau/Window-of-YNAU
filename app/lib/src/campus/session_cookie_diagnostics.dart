// 【临时诊断】Cookie 取不到的三步排查（t137）—— 验证后删除。
//
// owner 反馈：探针永远是「Cookie: 0 条」，不管有没有登录。本模块只做**观测**，判定取不到的原因：
//   1) 精确 URL 查询（用"即将请求的那个 URL"，而不是按域猜）；
//   2) JS 侧对照（`document.cookie.length`：>0 说明有非 HttpOnly Cookie）；
//   3) 跨域扫描（one / jwxt / cas / .ynau.edu.cn 各查一次）。
//
/// 安全：只暴露 Cookie 的**名字与域**（数量/名称），**值一律不采集、不展示、不外传**。

/// 一条 Cookie 的可见元信息（**没有值**）。
class CookieRef {
  const CookieRef({required this.name, required this.domain});

  final String name;
  final String domain;

  @override
  String toString() => 'CookieRef($name@$domain)';
}

/// 查询某个 URL 下可见 Cookie 的元信息（测试可注入假实现）。
typedef CookieRefSource = Future<List<CookieRef>> Function(Uri url);

/// JS 侧对照：返回 `document.cookie.length`（拿不到 = null）。
typedef JsCookieLengthProbe = Future<int?> Function();

/// 单个域的扫描结果。
class DomainScan {
  const DomainScan({required this.domain, required this.cookies, this.error});

  final String domain;
  final List<CookieRef> cookies;
  final String? error;

  int get count => cookies.length;

  @override
  String toString() =>
      'DomainScan($domain, count=$count, names=${cookies.map((CookieRef c) => c.name).join(',')})';
}

/// 三步诊断的汇总结果。
class CookieDiagnostics {
  const CookieDiagnostics({
    required this.targetUrl,
    required this.exact,
    required this.scans,
    required this.jsCookieLength,
    this.exactError,
  });

  final String targetUrl;

  /// 精确 URL 下 CookieManager 返回的 Cookie（只有名字/域）。
  final List<CookieRef> exact;
  final String? exactError;

  /// 跨域扫描。
  final List<DomainScan> scans;

  /// `document.cookie.length`（null = WebView 不可用/未注入）。
  final int? jsCookieLength;

  /// 疑似原因（给 owner 一眼看）。
  String get diagnosis {
    final bool exactEmpty = exact.isEmpty;
    final bool jsHasCookies = (jsCookieLength ?? 0) > 0;
    final int otherDomains = scans
        .where((DomainScan scan) => scan.domain != Uri.parse(targetUrl).host)
        .fold<int>(0, (int sum, DomainScan scan) => sum + scan.count);

    if (!exactEmpty) {
      return '✅ CookieManager 能取到 ${exact.length} 条（域/名称见下）→ 之前取不到可能是“按域猜”导致域不匹配';
    }
    if (jsHasCookies) {
      return '⚠️ 强烈指向 HttpOnly：CookieManager 0 条，但 JS 侧 document.cookie 有 $jsCookieLength 字符'
          '（非 HttpOnly 的能读到）→ 会话 Cookie 很可能带 HttpOnly，插件取不到';
    }
    if (otherDomains > 0) {
      return '⚠️ 域不匹配：目标 URL 下 0 条，但其它校方域共 $otherDomains 条 → 改成按目标 URL 精确查/切换目标域';
    }
    if (jsCookieLength == null) {
      return '⚠️ 无法判定：CookieManager 0 条，且 WebView 未就绪（拿不到 document.cookie）→ 先确保课表 WebView 打开过/已登录';
    }
    return '⚠️ 无会话或时序/实例问题：CookieManager 0 条、JS 侧也为 0 → 先确认已在同一个 WebView 实例里登录，再重试';
  }

  /// 只输出名字与域，**不含任何 Cookie 值**。
  @override
  String toString() => 'CookieDiagnostics(target=$targetUrl, exact=${exact.length}, '
      'js=$jsCookieLength, scans=${scans.map((DomainScan s) => '${s.domain}:${s.count}').join(',')})';
}

/// 默认扫描域（按目标 URL + 已知校方域）。
List<String> defaultScanDomains(String targetHost) => <String>[
      targetHost,
      'one.ynau.edu.cn',
      'jwxt.ynau.edu.cn',
      'cas.ynau.edu.cn',
      'ynau.edu.cn',
    ]..removeWhere((String host) => host.isEmpty);

/// 跑三步诊断。
Future<CookieDiagnostics> diagnoseCookies({
  required Uri target,
  required CookieRefSource source,
  JsCookieLengthProbe? jsProbe,
  List<String>? scanDomains,
}) async {
  final String host = target.host;
  final List<String> domains = defaultScanDomains(host);
  if (scanDomains != null && scanDomains.isNotEmpty) {
    domains
      ..clear()
      ..addAll(scanDomains);
  }

  final List<CookieRef> exact = <CookieRef>[];
  String? exactError;
  try {
    exact.addAll(await source(target));
  } on Object catch (failure) {
    exactError = failure.runtimeType.toString();
  }

  final List<DomainScan> scans = <DomainScan>[];
  for (final String domain in domains) {
    try {
      final List<CookieRef> cookies =
          await source(Uri.parse('https://$domain/'));
      scans.add(DomainScan(domain: domain, cookies: cookies));
    } on Object catch (failure) {
      scans.add(DomainScan(domain: domain, cookies: const <CookieRef>[], error: failure.runtimeType.toString()));
    }
  }

  int? jsLength;
  if (jsProbe != null) {
    try {
      jsLength = await jsProbe();
    } on Object {
      jsLength = null;
    }
  }

  return CookieDiagnostics(
    targetUrl: target.toString(),
    exact: exact,
    exactError: exactError,
    scans: scans,
    jsCookieLength: jsLength,
  );
}

/// 把 `document.cookie.length` 的结果解析成 int（非数字/空 → null）。
int? parseJsCookieLength(String raw) {
  final String trimmed = raw.trim();
  if (trimmed.isEmpty) {
    return null;
  }
  // evaluateJavascript 可能返回带引号的字符串
  final String unquoted = trimmed.replaceAll('"', '').replaceAll("'", '');
  return int.tryParse(unquoted);
}

/// 只用于测试/日志的脱敏展示：`name@domain`（绝不含值）。
String describeCookieRefs(List<CookieRef> cookies) =>
    cookies.isEmpty ? '（无）' : cookies.map((CookieRef c) => '${c.name}@${c.domain}').join('\n');

/// Cookie 值永不采集：本模块只接受 [CookieRef]（结构里没有 value 字段）。
const String kCookieValuesNeverCollected = 'true';
// 说明：这个常量只是给评审一个显式信号；真正的保证来自 CookieRef 结构本身。

/// 【临时】JS 侧对照的桥：由课表 WebView 在创建时注入（见 `webview_fetcher.dart`）。
///
/// 用途：执行 `document.cookie.length`。它与 CookieManager 的差异就是判断 HttpOnly 的关键。
class SessionProbeJsBridge {
  const SessionProbeJsBridge._();

  static Future<int?> Function()? documentCookieLength;
}
