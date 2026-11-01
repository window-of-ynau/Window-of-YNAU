// W6：JS bridge 状态机的单元测试（纯 Dart，不需要 WebView / 真账号）。

import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/src/campus/campus_bridge.dart';
import 'package:yunong/src/campus/webview_session.dart';
import 'package:yunong/src/core/timetable_service.dart';

/// 收集 bridge 回调的小工具。
class _Recorder {
  final List<FetchedPage> fetched = <FetchedPage>[];
  final List<FetchException> errors = <FetchException>[];
  final List<LoginProbe> probes = <LoginProbe>[];
  final List<FetchStage> stages = <FetchStage>[];

  late final CampusBridge bridge = CampusBridge(
    onFetched: fetched.add,
    onError: errors.add,
    onLoginState: probes.add,
    onStage: stages.add,
  );
}

void main() {
  group('登录态消息', () {
    test('logged_in → 回调 LoginProbe，不产生取数', () {
      final recorder = _Recorder();
      final consumed = recorder.bridge.handleRaw(
        '{"type":"login_state","state":"logged_in","reason":"timetable","path":"/x"}',
      );
      expect(consumed, isTrue);
      expect(recorder.probes.single.state, CampusLoginState.loggedIn);
      expect(recorder.probes.single.reason, 'timetable');
      expect(recorder.fetched, isEmpty);
      expect(recorder.errors, isEmpty);
    });

    test('need_login → 回调 LoginProbe(needLogin)', () {
      final recorder = _Recorder();
      recorder.bridge.handleRaw(
        '{"type":"login_state","state":"need_login","reason":"login_form"}',
      );
      expect(recorder.probes.single.state, CampusLoginState.needLogin);
    });

    test('旧脚本的 login_done 仍被识别为已登录', () {
      final recorder = _Recorder();
      recorder.bridge.handleRaw('{"type":"login_done"}');
      expect(recorder.probes.single.isLoggedIn, isTrue);
      expect(recorder.probes.single.reason, 'legacy_signal');
    });
  });

  group('取数消息', () {
    test('timetable → 交付一次，重复消息不再交付', () {
      final recorder = _Recorder();
      recorder.bridge.handleRaw('{"type":"timetable","html":"<table/>","path":"/t"}');
      recorder.bridge.handleRaw('{"type":"timetable","html":"<table/>","path":"/t"}');
      expect(recorder.fetched, hasLength(1));
      expect(recorder.stages, contains(FetchStage.grabbed));
    });

    test('空表格 → HTML_CHANGED（不冒充"这学期没课"）', () {
      final recorder = _Recorder();
      recorder.bridge.handleRaw('{"type":"timetable","html":"   "}');
      expect(recorder.fetched, isEmpty);
      expect(recorder.errors.single.code, 'HTML_CHANGED');
      expect(recorder.errors.single.userHint, contains('可能已更新'));
    });

    test('HTML_CHANGED 不锁定会话：后续真抓到表仍算成功（自动重试窗口）', () {
      final recorder = _Recorder();
      recorder.bridge.handleRaw(
        '{"type":"error","code":"HTML_CHANGED","message":"没找到表"}',
      );
      expect(recorder.errors, hasLength(1));

      recorder.bridge.handleRaw('{"type":"timetable","html":"<table/>"}');
      expect(recorder.fetched, hasLength(1), reason: '重试窗口内后续成功必须交付');
    });

    test('交付成功后的错误消息不再打扰用户', () {
      final recorder = _Recorder();
      recorder.bridge.handleRaw('{"type":"timetable","html":"<table/>"}');
      recorder.bridge.handleRaw('{"type":"error","code":"HTML_CHANGED"}');
      expect(recorder.errors, isEmpty);
    });
  });

  group('消息解析健壮性', () {
    test('未知 type / 非法 JSON / 非字符串 都不会崩', () {
      final recorder = _Recorder();
      expect(recorder.bridge.handleRaw(' {"type":"something_new"} '), isFalse);
      expect(recorder.bridge.handleRaw('not json'), isFalse);
      expect(recorder.bridge.handleRaw(const <int>[1, 2, 3]), isFalse);
      expect(recorder.bridge.handleRaw(null), isFalse);
      expect(recorder.bridge.handleRaw(''), isFalse);
      expect(recorder.errors, isEmpty);
      expect(recorder.fetched, isEmpty);
    });

    test('error 消息缺 code 时兜底成 HTML_CHANGED', () {
      final recorder = _Recorder();
      recorder.bridge.handleRaw('{"type":"error"}');
      expect(recorder.errors.single.code, 'HTML_CHANGED');
    });
  });

  group('失败码 → 用户提示', () {
    test('每个失败码都有可读文案，且不暴露异常栈', () {
      for (final code in <String>[
        'HTML_CHANGED',
        'LOGIN_TIMEOUT',
        'NO_WEBVIEW',
        'NETWORK',
        'CERT',
        'CANCELLED',
        'TIMEOUT',
        'SOMETHING_ELSE',
      ]) {
        final message = describeFetchFailure(code);
        expect(message, isNotEmpty);
        expect(message, isNot(contains('Exception')));
        expect(message, isNot(contains('Error:')));
      }
      expect(describeFetchFailure('LOGIN_TIMEOUT'), contains('重试'));
      expect(describeFetchFailure('NETWORK'), contains('校园网'));
    });

    test('网络/证书错误分类', () {
      expect(classifyWebViewError('certificate error'), 'CERT');
      expect(classifyWebViewError(null, type: 'serverCertificate'), 'CERT');
      expect(classifyWebViewError('net::ERR_NAME_NOT_RESOLVED'), 'NETWORK');
      expect(classifyWebViewError(null, type: 'hostLookup'), 'NETWORK');
      expect(classifyWebViewError('something odd'), 'NETWORK');
    });
  });
}
