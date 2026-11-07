// t161：校方域名硬守卫（**功能必需，不是调试设施**）。
//
// 校方域名（ssoo/one/jwxt/jwgl/cas.ynau.edu.cn）有动态 JS 防护（瑞数一类）：
// 412 响应体是 `document.createElement("section"); $_ts=window['$_ts'] …` 这类挑战脚本，
// 必须真实执行 JS 才能拿到响应。因此**禁止**从 Dart HTTP / 云端 / CLI 直连校方接口；
// 校方数据只在 WebView 内用 `fetch` 取。
//
// 这条守卫在 t161 清理调试设施时被保留下来：它是**功能**（禁止直连校方接口），不是诊断工具。

/// 校方域名（含子域判定用）。
const Set<String> kCampusHosts = <String>{
  'ssoo.ynau.edu.cn',
  'ssoo.ynu.edu.cn',
  'one.ynau.edu.cn',
  'jwxt.ynau.edu.cn',
  'jwgl.ynau.edu.cn',
  'cas.ynau.edu.cn',
  'ynau.edu.cn',
  'ynu.edu.cn',
};

/// 是否属于校方域名（含子域）。
bool isCampusHost(String host) {
  final String h = host.toLowerCase();
  for (final String campus in kCampusHosts) {
    if (h == campus || h.endsWith('.$campus')) {
      return true;
    }
  }
  return false;
}
