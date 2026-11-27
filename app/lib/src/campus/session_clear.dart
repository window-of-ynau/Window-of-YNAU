// security M3：清除校务会话（WebView2 用户数据），**不碰**课表/记忆数据库。
//
// 为什么要单独一层：设置页只负责交互；真正的删除要有「路径护栏」和可被单测替换的
// Cookie 清理回调，避免误删 `<数据根>` 本身或数据库文件。

import 'dart:io';

import 'package:flutter_inappwebview/flutter_inappwebview.dart';

import '../core/app_paths.dart';
import '../l10n/strings.dart';

class CampusSessionClearResult {
  const CampusSessionClearResult({
    required this.ok,
    this.removedEntries = 0,
    this.cookiesCleared = false,
    this.error,
  });

  final bool ok;
  final int removedEntries;
  final bool cookiesCleared;
  final String? error;
}

class CampusSessionClearService {
  const CampusSessionClearService({
    this.webViewDataDir,
    this.databasePath,
    this.clearCookies,
  });

  /// 测试可注入固定目录；生产走 [AppPaths.webViewDataDir]。
  final String? webViewDataDir;
  final String? databasePath;

  /// 测试可注入（验证"插件 clear 被调用"）；生产用 flutter_inappwebview 的 CookieManager。
  final Future<bool> Function()? clearCookies;

  Future<CampusSessionClearResult> clear() async {
    final String dataDir = AppPaths.dataDir;
    final String dbPath = databasePath ?? AppPaths.databasePath;
    final String webviewDir = webViewDataDir ?? AppPaths.webViewDataDir;
    final String sep = Platform.pathSeparator;

    // 护栏：只删 `<数据根>/webview2`，绝不删数据根 / 数据库 / 浏览器的其它目录。
    final bool insideDataRoot = webviewDir.startsWith('$dataDir$sep') && webviewDir != dataDir;
    final bool dbInsideWebview = dbPath.startsWith('$webviewDir$sep');
    if (!insideDataRoot || dbInsideWebview) {
      return const CampusSessionClearResult(
        ok: false,
        error: SettingsS.clearSessionBadPath,
      );
    }

    int removed = 0;
    try {
      final Directory dir = Directory(webviewDir);
      if (dir.existsSync()) {
        removed = dir.listSync(recursive: false).length;
        await dir.delete(recursive: true);
      }
    } on Object catch (error) {
      return CampusSessionClearResult(
        ok: false,
        removedEntries: removed,
        error: '$error',
      );
    }

    // 移动端 Cookie 由 WebView 管理，目录删不到；再清一次插件 Cookie。
    // 插件失败不推翻目录删除的结果（Windows 下主要靠 WebView2 目录）。
    bool cookies = false;
    try {
      cookies = await (clearCookies ?? _clearCookiesViaPlugin)();
    } on Object {
      cookies = false;
    }

    return CampusSessionClearResult(
      ok: true,
      removedEntries: removed,
      cookiesCleared: cookies,
    );
  }

  static Future<bool> _clearCookiesViaPlugin() async {
    await CookieManager.instance().deleteAllCookies();
    return true;
  }
}
