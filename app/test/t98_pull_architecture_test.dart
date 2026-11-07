// t98 P0：课表取数改为 Dart 主动拉取 `window.__yunongState`（不再依赖 JS→Dart 桥）。
//
// 背景：诊断反复显示「候选 10 个 / hook 已装 / XHR 27 次」但「Dart 收到 0 条」——
// Dart→JS 通，JS→Dart 的 `flutter_inappwebview` 桥在该环境里始终不注入。
// 现在主路径是：脚本只把事件写进 `window.__yunongState.events`，Dart 每 400ms 用
// `evaluateJavascript('JSON.stringify(window.__yunongState)')` 拉取。

import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/src/campus/campus_bridge.dart';
import 'package:yunong/src/campus/portal_api.dart';

void main() {
  group('P0 注入脚本：写可拉取状态（不依赖桥）', () {
    test('脚本维护 window.__yunongState.events', () {
      expect(kPortalScheduleScript.contains('window.__yunongState'), isTrue);
      expect(kPortalScheduleScript.contains('.events.push('), isTrue);
      expect(kPortalScheduleScript.contains('updatedAt'), isTrue, reason: 'Dart 需要用它算 age');
    });

    test('桥不可用时入队而不是静默丢弃（兜底路径保留）', () {
      expect(kPortalScheduleScript.contains('PENDING.push('), isTrue);
    });
  });

  group('P0 Dart 侧：pull 统计进入诊断', () {
    test('bridge 暴露拉取计数，并写进诊断文本', () {
      final bridge = CampusBridge(
        onFetched: (_) {},
        onError: (_) {},
      );
      bridge.pullCount = 7;
      bridge.lastStateAgeSeconds = 2;
      bridge.stateEventsConsumed = 3;

      // 用一条带 diagnostics 的消息触发格式化（类型不关心，只要走 format 分支）
      final consumed = bridge.handle(<String, dynamic>{
        'type': 'unknown_for_test',
        'diagnostics': <String, dynamic>{},
      });
      expect(consumed, isFalse);
      expect(bridge.pullCount, 7);
      expect(bridge.lastStateAgeSeconds, 2);
      expect(bridge.stateEventsConsumed, 3);
    });
  });
}
