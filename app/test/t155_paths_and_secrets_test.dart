// t155：数据根收口——dev/release 分离、WebView2 目录挪出 exe 同级、源码不得含硬编码 Key。

import 'dart:io';

import 'package:flutter/foundation.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/src/campus/webview_session.dart';
import 'package:yunong/src/core/app_paths.dart';

void main() {
  group('dev / release 数据目录分离（t155 P0-2）', () {
    test('debug → yunong-dev，release → yunong', () {
      const String base = r'C:\Users\fan\AppData\Local';
      expect(AppPaths.dataDirUnder(base, debug: true).endsWith('yunong-dev'), isTrue);
      expect(AppPaths.dataDirUnder(base, debug: false).endsWith('yunong'), isTrue);
      // 不能互相包含（避免"dev 目录其实是 release 的子串"这种误判）
      expect(
        AppPaths.dataDirUnder(base, debug: true),
        isNot(AppPaths.dataDirUnder(base, debug: false)),
      );
    });

    test('Windows 根目录解析：优先 LOCALAPPDATA', () {
      final String base = AppPaths.baseDirFrom(
        <String, String>{'LOCALAPPDATA': r'C:\Users\fan\AppData\Local', 'APPDATA': r'C:\Roaming'},
        isWindows: true,
      );
      expect(base, r'C:\Users\fan\AppData\Local');
      expect(AppPaths.dataDirUnder(base, debug: false), contains('yunong'));
    });

    test('当前进程：库文件与设置文件都在同一个数据根下', () {
      expect(AppPaths.databasePath, endsWith('yunong.db'));
      expect(AppPaths.dataDirOf(AppPaths.databasePath), AppPaths.dataDir);
      expect(AppPaths.settingsPath, endsWith('settings.json'));
      expect(AppPaths.dataDirOf(AppPaths.settingsPath), AppPaths.dataDir);
      // 测试跑在 debug 下
      expect(AppPaths.dataDir.endsWith(AppPaths.debugDirName), isTrue);
    });
  });

  group('WebView2 user data 目录（t155 P0-1）', () {
    test('显式路径 = <数据根>/webview2，且不在 exe 同级', () {
      const String base = r'C:\Users\fan\AppData\Local';
      final String release = AppPaths.webViewDataDirUnder(base, debug: false);
      final String dev = AppPaths.webViewDataDirUnder(base, debug: true);
      expect(release.replaceAll('\\', '/'), endsWith('yunong/webview2'));
      expect(dev.replaceAll('\\', '/'), endsWith('yunong-dev/webview2'));
      // 绝不能出现在 exe 同级（`<exe>.WebView2/EBWebView`）
      expect(release.contains('.WebView2'), isFalse);
      expect(dev.contains('.WebView2'), isFalse);
    });

    test('WebViewSession.userDataDir() 与数据根一致（不是 exe 同级）', () {
      final String dir = WebViewSession.userDataDir();
      expect(dir.endsWith('webview2'), isTrue, reason: '实际返回：$dir');
      expect(dir.contains('yunong'), isTrue);
      expect(dir.contains('.WebView2'), isFalse,
          reason: '不能落回 WebView2 的 `<exe>.WebView2` 默认目录');
      // 与 AppPaths 的推导一致
      expect(dir, AppPaths.webViewDataDir);
    });

    test('平台说明文本里给出真实路径（便于排查"会话丢了"）', () {
      // flutter_test 里 defaultTargetPlatform 默认是 android，这里显式模拟 Windows。
      debugDefaultTargetPlatformOverride = TargetPlatform.windows;
      try {
        expect(WebViewSession.storageDescription, contains('webview2'));
      } finally {
        debugDefaultTargetPlatformOverride = null;
      }
    });
  });

  group('硬编码密钥防线（t155 P0-3）', () {
    // 真 Key 形如 sk-<长串>；这里只拦"看起来像真 Key"的，不误伤示例文案。
    final RegExp keyPattern = RegExp(r'sk-[A-Za-z0-9]{16,}');
    // 允许出现在测试用例里的假 Key（全是重复字符或以 test/demo 开头）
    final RegExp allowlist = RegExp(r'sk-(test|demo|fake|dummy|xxxx)|sk-([a-z])\2{15,}');

    test('lib/ 与 assets/ 不含形如 sk- 的真实密钥', () {
      final List<String> hits = <String>[];
      for (final Directory root in <Directory>[
        Directory('lib'),
        Directory('assets'),
      ]) {
        if (!root.existsSync()) {
          continue;
        }
        for (final File file in root
            .listSync(recursive: true)
            .whereType<File>()
            .where((File f) => f.path.endsWith('.dart') || f.path.endsWith('.json') ||
                f.path.endsWith('.html') || f.path.endsWith('.yaml') || f.path.endsWith('.md'))) {
          final String text = file.readAsStringSync();
          for (final RegExpMatch match in keyPattern.allMatches(text)) {
            final String found = match.group(0)!;
            if (allowlist.hasMatch(found)) {
              continue;
            }
            hits.add('${file.path}: ${found.substring(0, 6)}…');
          }
        }
      }
      expect(hits, isEmpty, reason: '发现疑似硬编码 Key：${hits.join('；')}');
    });

    test('测试用的假 Key 会被 allowlist 放行（避免规则过严导致误报）', () {
      expect(allowlist.hasMatch('sk-test-1234'), isTrue);
      expect(keyPattern.hasMatch('sk-test-1234'), isFalse, reason: '太短的不会命中');
    });
  });
}
