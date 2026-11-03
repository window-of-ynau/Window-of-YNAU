// 应用数据路径（Windows / 其它平台）。
//
// 为什么需要它：凭据（DPAPI 密文）、会话库都必须落在**同一个可写目录**，
// 否则 `yunong_set_secret` 与 `yunong_chat_stream` 会各看各的目录，
// 表现就是"Key 保存了但发送时读不到"（t52 P0-1 的根因之一）。

import 'dart:io';

/// 端侧数据目录与库文件路径。
class AppPaths {
  const AppPaths._();

  /// 数据目录：Windows 用 `%LOCALAPPDATA%\yunong`，其它平台用 `$HOME/.local/share/yunong`。
  static String get dataDir {
    final env = Platform.environment;
    if (Platform.isWindows) {
      final base = env['LOCALAPPDATA'] ?? env['APPDATA'] ?? env['USERPROFILE'] ?? '';
      if (base.isNotEmpty) {
        return <String>[base, 'yunong'].join(Platform.pathSeparator);
      }
    } else {
      final home = env['HOME'] ?? '';
      if (home.isNotEmpty) {
        return <String>[home, '.local', 'share', 'yunong'].join(Platform.pathSeparator);
      }
    }
    // 兜底：当前目录下的 .yunong（至少读写一致）
    return <String>[Directory.current.path, '.yunong'].join(Platform.pathSeparator);
  }

  /// 会话/课表共用的 SQLite 库；凭据密文放在同目录的 `secrets/`。
  static String get databasePath => <String>[dataDir, 'yunong.db'].join(Platform.pathSeparator);
}
