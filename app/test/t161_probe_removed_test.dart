// t161：调试探针下线（#55）后的零命中护栏 + 关键路径可观测（#54）。

import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/src/campus/campus_bridge.dart';
import 'package:yunong/src/campus/campus_guard.dart';
import 'package:yunong/src/core/timetable_service.dart';

void main() {
  group('调试探针已下线（#55）', () {
    test('app/lib 内 `session_probe` / `[临时]` 零命中', () {
      final List<String> hits = <String>[];
      for (final File file in Directory('lib').listSync(recursive: true).whereType<File>()) {
        if (!file.path.endsWith('.dart')) {
          continue;
        }
        final String text = file.readAsStringSync();
        for (final String needle in <String>['session_probe', '[临时]', 'SessionProbe']) {
          if (text.contains(needle)) {
            hits.add('${file.path} 含「$needle」');
          }
        }
      }
      expect(hits, isEmpty, reason: hits.join('；'));
    });

    test('探针相关文件已删除（代码不留）', () {
      for (final String gone in <String>[
        'lib/src/campus/session_probe.dart',
        'lib/src/campus/session_probe_dialog.dart',
        'lib/src/campus/session_cookie_diagnostics.dart',
        'lib/src/campus/card_id_hints.dart',
      ]) {
        expect(File(gone).existsSync(), isFalse, reason: '$gone 应已删除');
      }
    });

    test('功能必需的东西仍在：校方域名硬守卫', () {
      expect(isCampusHost('one.ynau.edu.cn'), isTrue);
      expect(isCampusHost('ssoo.ynau.edu.cn'), isTrue);
      expect(isCampusHost('jwxt.ynau.edu.cn'), isTrue);
      expect(isCampusHost('example.com'), isFalse);
      expect(kCampusHosts, contains('ynau.edu.cn'));
    });
  });

  group('关键路径失败可观测（#54）', () {
    test('桥接安装失败 → BRIDGE_UNAVAILABLE + 明确提示，且写诊断', () {
      final List<FetchException> errors = <FetchException>[];
      final CampusBridge bridge = CampusBridge(onFetched: (_) {}, onError: errors.add);

      bridge.reportBridgeFailure(
        StateError('PlatformException(addJavaScriptHandler)'),
        where: 'addJavaScriptHandler',
      );

      expect(errors.length, 1);
      final FetchException error = errors.single;
      expect(error.code, 'BRIDGE_UNAVAILABLE');
      // 用户看得懂的一句话（不是异常栈）
      expect(error.userHint, contains('通道没建立起来'));
      expect(error.userHint, contains('重启应用'));
      // 诊断里带位置与原因，便于用户反馈
      expect(error.message, contains('addJavaScriptHandler'));
      expect(error.message, contains('StateError'));
      expect(bridge.lastHtmlDiagnostics, contains('桥接安装失败'));

      // 失败码也有对应的通俗说明
      expect(describeFetchFailure('BRIDGE_UNAVAILABLE'), contains('通道没建立起来'));
    });

    test('桥接失败不锁定后续成功路径？—— 它属于硬失败：后续 run 不再交付', () async {
      final List<FetchException> errors = <FetchException>[];
      final List<FetchedPage> fetched = <FetchedPage>[];
      final CampusBridge bridge = CampusBridge(onFetched: fetched.add, onError: errors.add);
      bridge.reportBridgeFailure(Exception('boom'));
      bridge.handle(<String, dynamic>{
        'type': 'timetable',
        'html': '<html><table id="timetable"><tr><td>课</td></tr></table>',
      });
      await Future<void>.delayed(const Duration(milliseconds: 5));
      expect(errors.length, 1, reason: '桥已判定不可用，不再重复报错');
      expect(fetched, isEmpty, reason: '通道没建立时不应假装抓取成功');
    });
  });
}
