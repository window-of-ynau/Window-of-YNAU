// 构建标识（t96）：让「测的是哪个版本」在界面上直接可见。
//
// 为什么需要它：`yunong.exe` 是 C++ runner 桩，时间戳永远旧；真正的 Dart 产物是
// `data/app.so`、Rust 是 `yunong_core.dll`。只看文件时间戳判断不了版本，
// 所以构建时把 commit 与打包时间注入进来。
//
// 注入方式（由 `scripts/build-windows-app.ps1` 传）：
//   --dart-define=BUILD_COMMIT=<git rev-parse --short HEAD>
//   --dart-define=BUILD_TIME=<打包时间>
// 未注入（本地 `flutter run`）→ 显示 `dev`。

abstract final class BuildInfo {
  /// 构建时的 git commit（短哈希）；未注入为 `dev`。
  static const String commit = String.fromEnvironment('BUILD_COMMIT', defaultValue: 'dev');

  /// 打包时间（人类可读，例如 `11-03 16:30`）；未注入为 `dev`。
  static const String time = String.fromEnvironment('BUILD_TIME', defaultValue: 'dev');

  /// 是否是有注入的正式构建。
  static bool get isRelease => commit != 'dev' || time != 'dev';

  /// 一行展示：`构建 abc1234 · 11-03 16:30`；未注入时 `构建 dev`。
  static String get label {
    if (!isRelease) {
      return '构建 dev';
    }
    final List<String> parts = <String>[
      if (commit != 'dev') '构建 $commit',
      if (time != 'dev') time,
    ];
    return parts.join(' · ');
  }
}
