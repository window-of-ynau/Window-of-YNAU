// 端到端冒烟（不需要 GUI）：真实 DLL → 解析 fixture → 落 SQLite → 查询。
//
// 用法（在 `app/` 下）：
//   dart run tool/verify_timetable_pipeline.dart
// 前置：先构建 DLL（`cargo build -p yunong-core` 或 --release），并用
// `YUNONG_CORE_DLL` 指向它；也可以用 `--dart-define` 之外的环境变量。
//
// 这个脚本是"没有 WebView 也能证明整条链路通"的确定性入口，CI 也能跑。

import 'dart:ffi';
import 'dart:io';

import 'package:ffi/ffi.dart';

typedef _ThreeC = Pointer<Utf8> Function(Pointer<Utf8>, Pointer<Utf8>, Pointer<Utf8>);
typedef _TwoC = Pointer<Utf8> Function(Pointer<Utf8>, Pointer<Utf8>);
typedef _FreeC = void Function(Pointer<Utf8>);
typedef _FreeNative = Void Function(Pointer<Utf8>);

String _callThree(_ThreeC fn, _FreeC free, List<String> args) {
  final allocated = args.map((String a) => a.toNativeUtf8()).toList();
  try {
    final out = fn(allocated[0], allocated[1], allocated[2]);
    try {
      return out.toDartString();
    } finally {
      free(out);
    }
  } finally {
    for (final Pointer<Utf8> p in allocated) {
      calloc.free(p);
    }
  }
}

void main() {
  final dllPath = Platform.environment['YUNONG_CORE_DLL'] ??
      (Platform.isWindows ? 'target/release/yunong_core.dll' : 'target/release/libyunong_core.so');
  final lib = DynamicLibrary.open(dllPath);
  final import = lib.lookupFunction<_ThreeC, _ThreeC>('yunong_import_timetable');
  final list = lib.lookupFunction<_TwoC, _TwoC>('yunong_list_courses');
  final free = lib.lookupFunction<_FreeNative, _FreeC>('yunong_free_string');

  final sample = File('assets/fixtures/timetable_sample.html').readAsStringSync();
  final revamp = File('assets/fixtures/timetable_revamp.html').readAsStringSync();
  final db = '${Directory.systemTemp.path}${Platform.pathSeparator}yunong-smoke${Platform.pathSeparator}timetable.db';

  stdout.writeln('[1] 导入正常课表：');
  stdout.writeln('    ${_callThree(import, free, <String>[sample, '2026-2027-1', db])}');

  stdout.writeln('[2] 查询课表：');
  final listed = _callThree(
    (Pointer<Utf8> a, Pointer<Utf8> b, Pointer<Utf8> _) => list(a, b),
    free,
    <String>['2026-2027-1', db, ''],
  );
  stdout.writeln('    $listed');

  stdout.writeln('[3] 导入改版页面（必须失败、不落库）：');
  stdout.writeln('    ${_callThree(import, free, <String>[revamp, '2026-2027-1', db])}');

  stdout.writeln('[4] 再次查询（旧数据必须还在）：');
  final again = _callThree(
    (Pointer<Utf8> a, Pointer<Utf8> b, Pointer<Utf8> _) => list(a, b),
    free,
    <String>['2026-2027-1', db, ''],
  );
  final kept = again.contains('高等数学A');
  stdout.writeln('    旧数据保留：$kept');

  final ok = listed.contains('高等数学A') && kept && again.contains('大学英语');
  stdout.writeln(ok ? 'SMOKE OK' : 'SMOKE FAILED');
  exit(ok ? 0 : 1);
}
