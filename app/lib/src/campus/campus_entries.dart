// 校务入口集中定义（原在 main.dart，W6 抽出来给取数层复用）。
//
// 只放"入口 URL"，不放凭据：凭据在 WebView 的会话里，见 `webview_session.dart`。

import 'package:flutter/material.dart';

/// 校务入口（ADR-07 / ADR-11：登录与取数都在 WebView 里完成）。
enum CampusEntry {
  portal('教务门户', 'https://jwxt.ynau.edu.cn/DefaultStu'),
  scores('成绩查询', 'https://jwgl.ynau.edu.cn/STU/cj_default.aspx'),
  // W6：课表走门户接口，入口是统一身份认证；登录后门户即 one.ynau.edu.cn/#/index5。
  timetable('我的课表', 'https://ssoo.ynau.edu.cn/lyuapServer/login'),
  library('图书馆', 'http://10.8.225.101:8013/#/Home'),
  software('正版软件', 'http://software.ynau.edu.cn/');

  const CampusEntry(this.label, this.url);

  final String label;
  final String url;

  /// 【#84】每个入口一枚独立的 Material Outlined 图标（20dp、单色、只用 token 上色）。
  ///
  /// 旧实现 `_Sidebar` 对所有入口共用 `Icons.school_outlined`，导致"图标全同、只能靠记忆认"。
  /// 映射见 `design/android-nav-icons.md` §2（codepoint 已对 Flutter 3.47.4 icons.dart 核对）。
  IconData get icon => switch (this) {
        CampusEntry.portal => Icons.account_balance_outlined,
        CampusEntry.scores => Icons.assessment_outlined,
        CampusEntry.timetable => Icons.calendar_month_outlined,
        CampusEntry.library => Icons.local_library_outlined,
        CampusEntry.software => Icons.verified_outlined,
      };
}
