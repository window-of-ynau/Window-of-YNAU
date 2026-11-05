// 应用数据路径（Windows / 其它平台）。
//
// 为什么需要它：凭据（DPAPI 密文）、会话库都必须落在**同一个可写目录**，
// 否则 `yunong_set_secret` 与 `yunong_chat_stream` 会各看各的目录，
// 表现就是"Key 保存了但发送时读不到"（t52 P0-1 的根因之一）。
//
// # t155 两条收口
//
// 1. **dev / release 数据分离**：debug 构建用 `yunong-dev`，release 用 `yunong`。
//    以前两者共用一份库：开发/测试会污染正式数据（owner 装完 release 看到开发期数据，
//    误以为"打包把测试环境带进去了"）。
// 2. **WebView2 用户数据必须挪出 exe 同级**：默认会写在 `<exe>.WebView2/EBWebView/`
//    （含 `Login Data`、Cookie 存储），既会被"整个目录打包"带进安装包，
//    也会在装到 `Program Files` 时因不可写而开不起 WebView。现在统一放 `<数据根>/webview2`。

import 'dart:io';

import 'package:flutter/foundation.dart';

/// 端侧数据目录与库文件路径。
class AppPaths {
  const AppPaths._();

  /// release 的数据目录名。
  static const String releaseDirName = 'yunong';

  /// debug（开发/测试）的数据目录名 —— **与 release 完全分开**。
  static const String debugDirName = 'yunong-dev';

  /// 数据目录名：debug 用 `yunong-dev`，release 用 `yunong`。
  static String dirNameFor({required bool debug}) => debug ? debugDirName : releaseDirName;

  /// 当前构建模式下的数据目录名。
  static String get dirName => dirNameFor(debug: kDebugMode);

  /// 某个根目录下的数据目录（纯函数，便于测试）。
  static String dataDirUnder(String base, {required bool debug}) =>
      <String>[base, dirNameFor(debug: debug)].join(Platform.pathSeparator);

  /// WebView2 用户数据目录（纯函数）：`<数据根>/webview2`（dev 与 release 也互不干扰）。
  static String webViewDataDirUnder(String base, {required bool debug}) =>
      <String>[dataDirUnder(base, debug: debug), 'webview2'].join(Platform.pathSeparator);

  /// 系统给的"每用户可写根"：Windows 用 `%LOCALAPPDATA%`，其它平台用 `$HOME/.local/share`。
  static String baseDirFrom(Map<String, String> env, {bool? isWindows}) {
    final bool windows = isWindows ?? Platform.isWindows;
    if (windows) {
      return env['LOCALAPPDATA'] ?? env['APPDATA'] ?? env['USERPROFILE'] ?? '';
    }
    final String home = env['HOME'] ?? '';
    return home.isEmpty ? '' : <String>[home, '.local', 'share'].join(Platform.pathSeparator);
  }

  /// 数据目录：`%LOCALAPPDATA%\yunong[-dev]` / `$HOME/.local/share/yunong[-dev]`。
  static String get dataDir {
    final String base = baseDirFrom(Platform.environment);
    if (base.isNotEmpty) {
      return dataDirUnder(base, debug: kDebugMode);
    }
    // 兜底：当前目录下的 `.yunong[-dev]`（至少读写一致；**不退回 exe 同级**）
    return <String>[Directory.current.path, '.$dirName'].join(Platform.pathSeparator);
  }

  /// 从库文件路径推出**目录**（Rust 的 `data_dir` 是目录，不是文件）。
  static String dataDirOf(String dbPath) {
    if (dbPath.isEmpty) {
      return '';
    }
    final int index = dbPath.lastIndexOf(RegExp(r'[/\\]'));
    return index <= 0 ? dbPath : dbPath.substring(0, index);
  }

  /// 会话/课表共用的 SQLite 库；凭据密文放在同目录的 `secrets/`。
  static String get databasePath => <String>[dataDir, 'yunong.db'].join(Platform.pathSeparator);

  /// WebView2 用户数据目录（**显式指定**，绝不用 exe 同级的默认值）。
  static String get webViewDataDir {
    final String base = baseDirFrom(Platform.environment);
    if (base.isNotEmpty) {
      return webViewDataDirUnder(base, debug: kDebugMode);
    }
    return <String>[Directory.current.path, '.$dirName', 'webview2'].join(Platform.pathSeparator);
  }

  /// 设置文件（`settings.json`）与数据根同目录。
  static String get settingsPath =>
      <String>[dataDir, 'settings.json'].join(Platform.pathSeparator);
}
