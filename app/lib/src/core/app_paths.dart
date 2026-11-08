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
import 'package:path_provider/path_provider.dart';

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
  ///
  /// t179：**必须归一化** —— Android 上 `HOME=/` 曾拼出 `//yunong`（不可写 → STORAGE 错误）。
  /// 这里折叠重复分隔符、去掉尾部多余分隔符，任何情况下都不会产出以 `//` 开头的路径。
  static String dataDirUnder(String base, {required bool debug}) {
    final String cleaned = normalizeBase(base);
    final String name = dirNameFor(debug: debug);
    return cleaned.isEmpty ? name : <String>[cleaned, name].join(Platform.pathSeparator);
  }

  /// 归一化根目录：折叠重复分隔符（`//` → `/`）、去掉尾部 `/`（保留 `C:\` 这种盘符根）。
  static String normalizeBase(String base) {
    String out = base.trim();
    if (out.isEmpty) {
      return '';
    }
    final String sep = Platform.pathSeparator;
    final String doubled = '$sep$sep';
    while (out.contains(doubled)) {
      out = out.replaceAll(doubled, sep);
    }
    while (out.length > 1 && out.endsWith(sep) && !(out.length == 3 && out.endsWith(':$sep'))) {
      out = out.substring(0, out.length - 1);
    }
    return out;
  }


  /// WebView2 用户数据目录（纯函数）：`<数据根>/webview2`（dev 与 release 也互不干扰）。
  static String webViewDataDirUnder(String base, {required bool debug}) =>
      <String>[dataDirUnder(base, debug: debug), 'webview2'].join(Platform.pathSeparator);

  /// 系统给的"每用户可写根"：Windows 用 `%LOCALAPPDATA%`，其它平台用 `$HOME/.local/share`。
  static String baseDirFrom(Map<String, String> env, {bool? isWindows, bool? isMobile}) {
    final bool windows = isWindows ?? Platform.isWindows;
    final bool mobile = isMobile ?? (Platform.isAndroid || Platform.isIOS);
    if (windows) {
      return env['LOCALAPPDATA'] ?? env['APPDATA'] ?? env['USERPROFILE'] ?? '';
    }
    // t179：Android/iOS 的 HOME 往往是 `/`，拼出来是 `//yunong`（不可写）→ 手机一律走平台目录
    if (mobile) {
      return '';
    }
    final String home = env['HOME'] ?? '';
    return home.isEmpty ? '' : <String>[home, '.local', 'share'].join(Platform.pathSeparator);
  }

  /// 【t174】平台可写根：Android/iOS 用 `getApplicationSupportDirectory()`（path_provider）。
  ///
  /// 背景：手机上 `LOCALAPPDATA`/`APPDATA`/`USERPROFILE` **都不存在**，旧实现会拼出
  /// `//yunong` 这种非法路径（owner 截图里的「数据目录：//yunong（默认）」）。
  static String? _platformBase;
  static bool _platformResolved = false;

  /// 解析一次平台目录（`main()` 里 await；测试可直接注入）。
  static Future<void> initPlatformDirs() async {
    if (_platformResolved) {
      return;
    }
    _platformResolved = true;
    if (!Platform.isAndroid && !Platform.isIOS) {
      return;
    }
    try {
      final Directory dir = await getApplicationSupportDirectory();
      _platformBase = dir.path;
      _platformBaseError = null;
      debugPrint('[paths] 手机数据根 = ${dir.path}');
    } on Object catch (failure) {
      _platformBaseError = '无法获取应用私有目录（path_provider）：'
          '${failure.runtimeType} $failure';
      debugPrint('[paths] getApplicationSupportDirectory 失败：$_platformBaseError');
    }
  }

  /// t179：手机取不到应用私有目录时的明确错误（**不再静默回落 `//yunong`**）。
  static String? get platformBaseError => _platformBaseError;

  static String? _platformBaseError;

  /// 纯函数：手机分支缺少平台目录 → 返回错误文案（供 UI 与测试断言）。
  static String? mobileRootProblem({required bool isMobile, required String? platformBase}) {
    if (!isMobile) {
      return null;
    }
    final String base = (platformBase ?? '').trim();
    if (base.isEmpty) {
      return '无法获取应用私有目录（path_provider 未就绪）；请重启应用或重新安装';
    }
    return null;
  }

  /// 测试/启动注入。
  @visibleForTesting
  static void debugSetPlatformBase(String? path) {
    _platformBase = path;
    _platformResolved = true;
  }

  /// 默认数据目录（没有覆盖时）：桌面用环境变量根；**手机只用应用私有目录**。
  ///
  /// t179（P0）：手机上 `HOME=/` 曾把数据根拼成 `//yunong`（不可写 → STORAGE 错误）。
  /// 现在手机分支**绝不**用环境变量根；取不到平台目录时交给 [dataRootError] 明确报错。
  static String get defaultDataDir {
    final bool mobile = Platform.isAndroid || Platform.isIOS;
    final String base = baseDirFrom(Platform.environment);
    if (!mobile && base.isNotEmpty) {
      return dataDirUnder(base, debug: kDebugMode);
    }
    final String? platform = _platformBase;
    if (platform != null && platform.isNotEmpty) {
      return dataDirUnder(platform, debug: kDebugMode);
    }
    // 兜底：当前目录下的 `.yunong[-dev]`（至少读写一致；**不退回 exe 同级**）
    return <String>[Directory.current.path, '.$dirName'].join(Platform.pathSeparator);
  }

  /// 数据目录：**优先**用 `--data-dir` / `YUNONG_DATA_DIR` 的覆盖值，否则用默认。
  static String get dataDir => _overridePath ?? defaultDataDir;

  /// 覆盖来源（默认 = 没有覆盖）。
  static DataRootSource get overrideSource => _overrideSource;

  /// 是否处于"便携/隔离"模式（命令行或环境变量指定了数据根）。
  static bool get isOverridden => _overrideSource != DataRootSource.defaultLocation;

  /// 覆盖路径初始化失败时的错误（非 null 时 UI 必须显示并拒绝启动业务）。
  static String? get dataRootError => _dataRootError ?? _platformBaseError;

  static String? _overridePath;
  static DataRootSource _overrideSource = DataRootSource.defaultLocation;
  static String? _dataRootError;

  /// **启动时调用一次**：解析 `--data-dir` / `YUNONG_DATA_DIR`。
  ///
  /// Flutter Windows 下 `main(List<String> args)` 拿不到 release 的启动参数，
  /// 只有 [Platform.executableArguments] 可靠 —— 这里把两者都收进来（命令行优先）。
  static void initDataRoot({
    Map<String, String>? env,
    List<String>? args,
    bool? debug,
  }) {
    final DataRootResolution resolution = resolveDataRoot(
      env: env ?? Platform.environment,
      args: args ?? Platform.executableArguments,
      defaultPath: defaultDataDir,
      debug: debug ?? kDebugMode,
    );
    _overrideSource = resolution.source;
    _dataRootError = resolution.error;
    _overridePath = resolution.source == DataRootSource.defaultLocation ? null : resolution.path;
  }

  /// 纯函数：解析三级优先级的覆盖路径并检查可写（便于测试）。
  ///
  /// 非法路径**不回退默认**：返回带 `error` 的结果，由 UI 明确报错。
  static DataRootResolution resolveDataRoot({
    required Map<String, String> env,
    required List<String> args,
    required String defaultPath,
    bool debug = false,
  }) {
    final String? fromArgs = _dataDirFromArgs(args);
    final String? fromEnv = _nonEmpty(env['YUNONG_DATA_DIR']);
    final String? requested = fromArgs ?? fromEnv;
    final DataRootSource source = fromArgs != null
        ? DataRootSource.commandLine
        : (fromEnv != null ? DataRootSource.environment : DataRootSource.defaultLocation);

    if (requested == null) {
      return DataRootResolution(
        path: defaultPath,
        source: DataRootSource.defaultLocation,
      );
    }

    final String dir = requested.trim();
    if (dir.isEmpty) {
      return DataRootResolution(
        path: defaultPath,
        source: source,
        error: '数据目录参数为空：$dir',
      );
    }
    try {
      final Directory target = Directory(dir);
      target.createSync(recursive: true);
      // 真正写一次（父目录存在 != 可写）
      final File probe = File(<String>[target.path, '.yunong-write-test'].join(Platform.pathSeparator));
      probe.writeAsStringSync('ok');
      probe.deleteSync();
    } on Object catch (failure) {
      return DataRootResolution(
        path: dir,
        source: source,
        error: '数据目录不可写：$dir（${failure.runtimeType}）',
      );
    }
    return DataRootResolution(path: dir, source: source);
  }

  static String? _dataDirFromArgs(List<String> args) {
    for (int i = 0; i < args.length; i++) {
      final String arg = args[i];
      if (arg == '--data-dir' && i + 1 < args.length) {
        return _nonEmpty(args[i + 1]);
      }
      if (arg.startsWith('--data-dir=')) {
        return _nonEmpty(arg.substring('--data-dir='.length));
      }
    }
    return null;
  }

  static String? _nonEmpty(String? value) {
    final String trimmed = (value ?? '').trim();
    return trimmed.isEmpty ? null : trimmed;
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
  ///
  /// t156：覆盖数据根时跟随覆盖值（`<覆盖目录>/webview2`），而不是 `%LOCALAPPDATA%`。
  static String get webViewDataDir {
    if (_overridePath != null) {
      return <String>[_overridePath!, 'webview2'].join(Platform.pathSeparator);
    }
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
/// 数据根来源：命令行 > 环境变量 > 默认位置。
enum DataRootSource {
  commandLine('命令行'),
  environment('环境变量'),
  defaultLocation('默认');

  const DataRootSource(this.label);

  final String label;
}

/// 数据根解析结果：路径 + 来源 + （可选）错误。
///
/// 非法路径**不回退默认**：业务必须看到 [error] 并明确报错退出。
class DataRootResolution {
  const DataRootResolution({required this.path, required this.source, this.error});

  final String path;
  final DataRootSource source;
  final String? error;

  bool get ok => error == null;
  bool get isOverride => source != DataRootSource.defaultLocation;
}
