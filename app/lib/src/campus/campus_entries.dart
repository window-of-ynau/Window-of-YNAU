// 校务入口集中定义（原在 main.dart，W6 抽出来给取数层复用）。
//
// 只放"入口 URL"，不放凭据：凭据在 WebView 的会话里，见 `webview_session.dart`。

/// 校务入口（ADR-07 / ADR-11：登录与取数都在 WebView 里完成）。
enum CampusEntry {
  portal('教务门户', 'https://jwxt.ynau.edu.cn/DefaultStu'),
  scores('成绩查询', 'https://jwgl.ynau.edu.cn/STU/cj_default.aspx'),
  timetable('我的课表', 'https://qzjwxt.ynau.edu.cn/jsxsd/framework/xsMainV.htmlx'),
  library('图书馆', 'http://10.8.225.101:8013/#/Home'),
  software('正版软件', 'http://software.ynau.edu.cn/');

  const CampusEntry(this.label, this.url);

  final String label;
  final String url;
}
