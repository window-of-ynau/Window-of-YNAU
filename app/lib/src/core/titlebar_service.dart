// 标题栏跟随主题（t84）：接 Rust `yunong_set_titlebar_dark(int32_t dark)`。
//
// # 契约（Rust 侧 t75 已实现并用像素实测）
// ```
// char* yunong_set_titlebar_dark(int32_t dark);
//   {"ok":true,"data":{"dark":true,"attribute":20,"hwnd":...}}
//   {"ok":false,"code":"UNSUPPORTED"}   非 Windows（忽略）
//   {"ok":false,"code":"NOT_FOUND"}     窗口还没建（正常时序，忽略/重试一次）
// ```
//
// # 所有权约定（与 chat 回调同一条规则）
// 返回值指针由 Rust `into_raw` 移交所有权：**必须先 `toDartString()` 拷贝，再 `yunong_free_string()`**。
// 不释放会泄漏；重复释放是 UB。一个指针只 free 一次。
//
// # 失败策略
// 标题栏只是装饰：任何失败（符号缺失、非 Windows、窗口未就绪、JSON 异常）**都不得影响启动或其它功能**。

import 'dart:convert';
import 'dart:ffi';

import 'package:ffi/ffi.dart';

import 'ffi_bindings.dart';
import '../theme/theme_controller.dart';

/// 抽象：测试可注入，不需要真 DLL。
abstract class TitleBarBackend {
  /// 返回 Rust 给的 code（`''` 表示成功）；实现不得抛异常。
  Future<String> setDark(bool dark);
}

/// 真 FFI 实现。
class FfiTitleBarBackend implements TitleBarBackend {
  FfiTitleBarBackend() : _lib = CoreLibrary.openCoreLibrary();

  final DynamicLibrary _lib;

  @override
  Future<String> setDark(bool dark) async {
    try {
      final Pointer<Utf8> Function(int) setDarkFn;
      final void Function(Pointer<Utf8>) freeFn;
      try {
        setDarkFn = _lib.lookupFunction<Pointer<Utf8> Function(Int32), Pointer<Utf8> Function(int)>(
            'yunong_set_titlebar_dark');
        freeFn = _lib.lookupFunction<Void Function(Pointer<Utf8>), void Function(Pointer<Utf8>)>(
            'yunong_free_string');
      } on ArgumentError {
        return 'UNSUPPORTED';
      }
      final Pointer<Utf8> result = setDarkFn(dark ? 1 : 0);
      if (result == nullptr) {
        return 'UNSUPPORTED';
      }
      // 所有权：先拷贝，再释放（只 free 一次）
      final String json = result.toDartString();
      freeFn(result);
      final decoded = jsonDecode(json);
      if (decoded is Map<String, dynamic>) {
        if (decoded['ok'] == true) {
          return '';
        }
        final code = decoded['code'];
        if (code is String) {
          return code;
        }
      }
      return 'UNKNOWN';
    } on Object {
      // 任何异常都不得冒泡到启动流程
      return 'UNKNOWN';
    }
  }
}

/// 主题 → 标题栏明暗：只有深蓝玻璃是深色。
bool titleBarDarkFor(AppThemeId id) => id == AppThemeId.deepGlass;

/// 把主题同步到原生标题栏；`NOT_FOUND`（窗口未建）重试一次。
Future<void> syncTitleBar(TitleBarBackend backend, AppThemeId id, {Future<void> Function(Duration)? sleep}) async {
  final dark = titleBarDarkFor(id);
  final code = await backend.setDark(dark);
  if (code == 'NOT_FOUND') {
    // 启动早期窗口还没建：稍后重试一次即可，不必提示用户
    await (sleep ?? Future<void>.delayed)(const Duration(milliseconds: 400));
    await backend.setDark(dark);
  }
}
