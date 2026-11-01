// W6：登录态探针 + 会话目录的纯 Dart 测试（CI 可跑，不需要 WebView/真账号）。
//
// 覆盖点：
// - 探针脚本**不读 Cookie**（隐私红线的机器可验证版本）；
// - 登录态三态映射、未知值兜底、path 去掉 query/fragment（CAS ticket 是凭据）；
// - 会话目录落在应用数据目录而不是临时目录。

import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/src/campus/webview_session.dart';

void main() {
  group('parseLoginProbe', () {
    test('已登录（课表表头）', () {
      final probe = parseLoginProbe(<String, dynamic>{
        'type': 'login_state',
        'state': 'logged_in',
        'reason': 'timetable',
        'path': '/jsxsd/framework/xsMainV.htmlx',
      });
      expect(probe.state, CampusLoginState.loggedIn);
      expect(probe.isLoggedIn, isTrue);
      expect(probe.hint, contains('正在读取课表'));
    });

    test('需要登录（登录表单）', () {
      final probe = parseLoginProbe(<String, dynamic>{
        'state': 'need_login',
        'reason': 'login_form',
      });
      expect(probe.state, CampusLoginState.needLogin);
      expect(probe.hint, contains('完成登录'));
    });

    test('未知状态不会误判成已登录', () {
      final probe = parseLoginProbe(<String, dynamic>{
        'state': 'whatever',
        'reason': '',
      });
      expect(probe.state, CampusLoginState.unknown);
      expect(probe.reason, 'no_signal');
      expect(probe.isLoggedIn, isFalse);
      expect(probe.hint, contains('没检测到登录状态'));
    });

    test('path 去掉 query 与 fragment（CAS ticket 属于凭据）', () {
      final probe = parseLoginProbe(<String, dynamic>{
        'state': 'logged_in',
        'reason': 'timetable',
        'path': '/jsxsd/xsMainV.htmlx?ticket=ST-123456-abc#top',
      });
      expect(probe.path, '/jsxsd/xsMainV.htmlx');
      expect(probe.path, isNot(contains('ticket')));
    });

    test('空 path 不报错', () {
      final probe = parseLoginProbe(<String, dynamic>{'state': 'need_login'});
      expect(probe.path, '');
    });
  });

  group('探针脚本隐私', () {
    test('脚本不读取 Cookie、不上报表单值', () {
      expect(kCampusProbeScript, isNot(contains('document.cookie')));
      expect(kCampusProbeScript, isNot(contains('cookie')));
      expect(kCampusProbeScript, isNot(contains('localStorage.getItem')));
      // 只回传登录态与课表 HTML
      expect(kCampusProbeScript, contains('login_state'));
      expect(kCampusProbeScript, contains("type: 'timetable'"));
      expect(kCampusProbeScript, contains('HTML_CHANGED'));
    });

    test('脚本优先用业务元素判定（抗改版）', () {
      // 课表表格 / 学号姓名栏 / iframe 内课表：三类信号都要在
      expect(kCampusProbeScript, contains('table#timetable'));
      expect(kCampusProbeScript, contains('#xh_id'));
      expect(kCampusProbeScript, contains('timetable_iframe'));
      // 登录表单作为"需要登录"的证据
      expect(kCampusProbeScript, contains("input[type=password]"));
    });
  });

  group('会话目录', () {
    test('落在应用数据目录，不是临时目录', () {
      final dir = WebViewSession.userDataDir();
      expect(dir, contains('yunong'));
      expect(dir, endsWith('webview'));
      expect(Directory(dir).existsSync(), isTrue);
      final temp = Directory.systemTemp.path;
      if (!temp.startsWith(Platform.environment['LOCALAPPDATA'] ?? '\u0000')) {
        expect(dir.startsWith(temp), isFalse, reason: '会话目录不能放在临时目录');
      }
    });

    test('storageDescription 不含任何 Cookie 值', () {
      final description = WebViewSession.storageDescription;
      expect(description, isNot(contains('cookie=')));
      expect(description.toLowerCase(), isNot(contains('sessionid')));
    });

    test('平台能力声明与实现一致', () {
      expect(WebViewSession.needsEnvironment, Platform.isWindows);
      expect(
        WebViewSession.isSupportedPlatform,
        Platform.isWindows || Platform.isAndroid || Platform.isIOS,
      );
    });
  });
}
