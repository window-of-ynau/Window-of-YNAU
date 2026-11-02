// R4-F1 回归测试：学期输入不得变成校务 origin 上的任意 JS。
//
// 漏洞：`window.__yunongTerm = '$term';`（单引号直接拼），`term = "x';globalThis.PWNED=1;//"`
// 会在 CAS 会话所在域执行任意代码。修复：格式校验 + `jsonEncode` 生成字符串字面量。

import 'dart:convert';

import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/src/campus/webview_fetcher.dart';
import 'package:yunong/src/core/timetable_service.dart' show FetchException;

const String _payload = "x';globalThis.PWNED=1;//";
const String _prefix = 'window.__yunongTerm = ';

void main() {
  // `_open` 会读 navigatorKey.currentState：没有 binding 会抛 FlutterError 而不是 FetchException
  TestWidgetsFlutterBinding.ensureInitialized();

  group('学期格式校验（入口先挡）', () {
    test('合法格式通过', () {
      for (final term in <String>['2026-2027-1', '2025-2026-2', '2030-2031-1']) {
        expect(isValidTermFormat(term), isTrue, reason: term);
      }
    });

    test('非法格式全部拒绝（含注入 payload）', () {
      for (final term in <String>[
        _payload,
        '2026-2027-3',
        '2026-2027',
        '2026-2027-1;',
        "x'",
        '',
        ' 2026-2027-1',
      ]) {
        expect(isValidTermFormat(term), isFalse, reason: term);
      }
    });
  });

  group('注入脚本生成', () {
    test('payload 被转义在字符串字面量内（jsonDecode 能原样取回）', () {
      final source = termAssignmentJs(_payload);
      expect(source, startsWith(_prefix));
      expect(source, endsWith(';'));
      final literal = source.substring(_prefix.length, source.length - 1);
      final decoded = jsonDecode(literal);
      expect(decoded, _payload, reason: '内容必须仍是同一个字符串，不能变成可执行代码');
    });

    test('普通学期生成的脚本可读且无多余转义', () {
      expect(termAssignmentJs('2026-2027-1'), 'window.__yunongTerm = "2026-2027-1";');
    });

    test('换行/反斜杠也被转义（不能跳出字符串）', () {
      final source = termAssignmentJs('a\nb\\c"d');
      final decoded = jsonDecode(source.substring(_prefix.length, source.length - 1));
      expect(decoded, 'a\nb\\c"d');
      expect(source.contains('\n'), isFalse, reason: '注入脚本里不能出现裸换行');
    });
  });

  group('fetchSchedule 入口拒绝非法学期', () {
    test('注入 payload → FetchException(BAD_INPUT)，不会打开 WebView', () async {
      final fetcher = WebViewSchoolFetcher();
      await expectLater(
        fetcher.fetchSchedule(entryUrl: 'https://one.ynau.edu.cn/', term: _payload),
        throwsA(isA<FetchException>().having((FetchException e) => e.code, 'code', 'BAD_INPUT')),
      );
    });

    test('合法学期不会被格式校验拦下（后续才可能报 NO_WEBVIEW）', () async {
      final fetcher = WebViewSchoolFetcher();
      await expectLater(
        Future<void>.sync(() async =>
            fetcher.fetchSchedule(entryUrl: 'https://one.ynau.edu.cn/', term: '2026-2027-1')),
        throwsA(isA<FetchException>().having(
          (FetchException e) => e.code,
          'code',
          isNot('BAD_INPUT'),
        )),
      );
    });
  });
}
