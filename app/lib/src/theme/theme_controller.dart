// 主题状态 + 持久化接口。
//
// 设计要点：
// 1. **接口按最终形态写**：`SettingsStore.get/set`。现在只有内存实现
//    （`MemorySettingsStore`），等 lead-client 的 core `settings` kv 表 + FFI
//    落地后，只换构造参数，UI 与 controller 一行不改。
// 2. 切换**即时生效**：`select()` 先 `notifyListeners()` 再异步落盘；
//    落盘失败只记日志，不回滚 UI（主题是体验偏好，不该因存储错误闪回）。
// 3. 主题 id 与 ThemeMode 分开存：前者决定"哪套视觉"，后者决定"明暗从哪来"。

import 'package:flutter/material.dart';

import 'themes.dart';

/// 三套主题。**默认 A（极简黑白）**。
enum AppThemeId {
  minimalBlack,
  deepGlass,
  paperCoral;

  static AppThemeId fromStorage(String? value) {
    switch (value) {
      case 'deep_glass':
        return AppThemeId.deepGlass;
      case 'paper_coral':
        return AppThemeId.paperCoral;
      case 'minimal_black':
      default:
        return AppThemeId.minimalBlack;
    }
  }

  String get storageValue {
    switch (this) {
      case AppThemeId.deepGlass:
        return 'deep_glass';
      case AppThemeId.paperCoral:
        return 'paper_coral';
      case AppThemeId.minimalBlack:
        return 'minimal_black';
    }
  }

  String get label {
    switch (this) {
      case AppThemeId.deepGlass:
        return '深蓝玻璃';
      case AppThemeId.paperCoral:
        return '书本质感';
      case AppThemeId.minimalBlack:
        return '极简黑白';
    }
  }

  String get hint {
    switch (this) {
      case AppThemeId.deepGlass:
        return '深蓝 + 玻璃质感';
      case AppThemeId.paperCoral:
        return '米白纸面 + 橘红';
      case AppThemeId.minimalBlack:
        return '灰度层级 + 细边框';
    }
  }

  /// 设置页预览用：当前主题下的一张"迷你页面"。
  ThemeData themeFor(Brightness brightness) => buildTheme(this, brightness);
}

/// 键名契约（与 lead-client 的 settings 表共用同一套键）。
abstract final class SettingsKeys {
  static const String themeId = 'ui.theme';
  static const String themeMode = 'ui.themeMode';
}

/// 设置存储。**实现方不要自己造第二套存储**：持久化由 core 的 settings 表提供。
abstract class SettingsStore {
  Future<String?> get(String key);

  Future<void> set(String key, String value);
}

/// 内存兜底：批次 1 用它把 UI 跑通；换成持久化实现时零改动。
class MemorySettingsStore implements SettingsStore {
  MemorySettingsStore([Map<String, String>? seed])
      : _values = <String, String>{...?seed};

  final Map<String, String> _values;

  @override
  Future<String?> get(String key) async => _values[key];

  @override
  Future<void> set(String key, String value) async => _values[key] = value;
}

/// 主题状态源：`MaterialApp` 由它驱动，切换不重启。
class ThemeController extends ChangeNotifier {
  ThemeController({
    SettingsStore? store,
    AppThemeId initialTheme = AppThemeId.minimalBlack,
    ThemeMode initialMode = ThemeMode.system,
  })  : _store = store ?? MemorySettingsStore(),
        _themeId = initialTheme,
        _themeMode = initialMode;

  final SettingsStore _store;
  AppThemeId _themeId;
  ThemeMode _themeMode;

  AppThemeId get themeId => _themeId;

  ThemeMode get themeMode => _themeMode;

  /// 从存储读回用户上次的选择（启动时调用一次；读失败保持默认）。
  Future<void> load() async {
    try {
      final String? id = await _store.get(SettingsKeys.themeId);
      final String? mode = await _store.get(SettingsKeys.themeMode);
      bool changed = false;
      if (id != null) {
        final AppThemeId parsed = AppThemeId.fromStorage(id);
        if (parsed != _themeId) {
          _themeId = parsed;
          changed = true;
        }
      }
      if (mode != null) {
        final ThemeMode parsed = _parseMode(mode);
        if (parsed != _themeMode) {
          _themeMode = parsed;
          changed = true;
        }
      }
      if (changed) {
        notifyListeners();
      }
    } catch (error) {
      debugPrint('主题偏好读取失败（保持默认）：$error');
    }
  }

  /// 切换主题：**先通知（即时生效）再落盘**。
  Future<void> select(AppThemeId id) async {
    if (id == _themeId) {
      return;
    }
    _themeId = id;
    notifyListeners();
    await _persist(SettingsKeys.themeId, id.storageValue);
  }

  Future<void> setThemeMode(ThemeMode mode) async {
    if (mode == _themeMode) {
      return;
    }
    _themeMode = mode;
    notifyListeners();
    await _persist(SettingsKeys.themeMode, _modeValue(mode));
  }

  /// 给 `MaterialApp` 用的两套 ThemeData（同一个主题 id、明/暗两档）。
  ThemeData theme(Brightness brightness) => buildTheme(_themeId, brightness);

  Future<void> _persist(String key, String value) async {
    try {
      await _store.set(key, value);
    } catch (error) {
      // 落盘失败不回滚：主题是体验偏好，闪回比存不上更难受。
      debugPrint('主题偏好保存失败：$error');
    }
  }

  static String _modeValue(ThemeMode mode) {
    switch (mode) {
      case ThemeMode.light:
        return 'light';
      case ThemeMode.dark:
        return 'dark';
      case ThemeMode.system:
        return 'system';
    }
  }

  static ThemeMode _parseMode(String value) {
    switch (value) {
      case 'light':
        return ThemeMode.light;
      case 'dark':
        return ThemeMode.dark;
      default:
        return ThemeMode.system;
    }
  }
}
