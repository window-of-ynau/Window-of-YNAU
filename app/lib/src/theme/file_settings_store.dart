// 主题/外观设置的落盘实现（t70 P0-3）。
//
// # 为什么写 JSON 文件而不是 core SQLite
//
// `settings` 只是"UI 偏好"（主题 id / 玻璃档位），**不是业务数据**：
// - 写 `%LOCALAPPDATA%\yunong\settings.json` 不用改 Rust、不用加 FFI、不会有 DB schema 迁移问题；
// - 主题是启动即需要的，文件能在 `runApp` 前**同步**读出，避免"先渲染默认主题再闪一下"；
// - 敏感信息仍然只走 DPAPI（`yunong_set_secret`），这里**绝不存任何凭据**。
//
// 失败策略：读写失败一律当作"没有设置"（返回 null / 静默忽略），
// 绝不让偏好文件损坏影响应用启动。

import 'dart:convert';
import 'dart:io';

import '../core/app_paths.dart';
import 'theme_controller.dart';

/// 把 `SettingsStore` 落到 `<LOCALAPPDATA>\yunong\settings.json`。
class FileSettingsStore implements SettingsStore {
  FileSettingsStore({String? path, Map<String, String>? seed})
      : _file = File(path ?? _defaultPath()) {
    if (seed != null) {
      _values.addAll(seed);
    }
    _loadFromDisk();
  }

  // t155：统一走 `AppPaths.settingsPath`（数据根：debug=`yunong-dev`、release=`yunong`）。
  static String _defaultPath() => AppPaths.settingsPath;

  final File _file;
  final Map<String, String> _values = <String, String>{};

  void _loadFromDisk() {
    try {
      if (!_file.existsSync()) {
        return;
      }
      final text = _file.readAsStringSync();
      if (text.trim().isEmpty) {
        return;
      }
      final decoded = jsonDecode(text);
      if (decoded is Map<String, dynamic>) {
        decoded.forEach((String key, dynamic value) {
          if (value is String) {
            _values[key] = value;
          }
        });
      }
    } on Object {
      // 文件损坏/无权限：按"没有设置"处理（不影响启动）
    }
  }

  void _flush() {
    try {
      final directory = _file.parent;
      if (!directory.existsSync()) {
        directory.createSync(recursive: true);
      }
      _file.writeAsStringSync(jsonEncode(_values), flush: true);
    } on Object {
      // 落盘失败不影响本次会话（下次启动会回到默认）
    }
  }

  @override
  Future<String?> get(String key) async => _values[key];

  @override
  Future<void> set(String key, String value) async {
    _values[key] = value;
    _flush();
  }
}
