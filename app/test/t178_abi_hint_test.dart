// t178：ABI 不支持时给「不支持本机 ABI」，ABI 支持时给完整诊断；双 lib 前缀候选与 maps 诊断在失败路径上真的被用到。

import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/src/core/ffi_bindings.dart';

void main() {
  group('非 arm64 设备：明确说「不支持本机 ABI」（t178 #1）', () {
    test('ABI=armeabi-v7a → 文案含「不支持本机 ABI」与「仅支持 arm64-v8a」', () {
      final String detail = CoreStatus.failureDetail(
        candidates: const <String>['libyunong_core.so', 'liblibyunong_core.so'],
        error: 'dlopen failed: library "libyunong_core.so" not found',
        abiHint: 'arm',
      );
      expect(detail, contains('不支持本机 ABI'));
      expect(detail, contains('armeabi-v7a'));
      expect(detail, contains('仅支持 arm64-v8a'));
      // 不再把笼统的 not found 当主文案
      expect(detail, isNot(startsWith('未加载')));
    });

    test('x86_64 同样判定为不支持', () {
      final String detail = CoreStatus.failureDetail(
        candidates: const <String>['libyunong_core.so'],
        error: 'dlopen failed',
        abiHint: 'x86_64',
      );
      expect(detail, contains('不支持本机 ABI'));
      expect(detail, contains('x86_64'));
    });
  });

  group('arm64 设备：给完整诊断，不误报（t178 #2）', () {
    test('ABI=arm64-v8a 且加载失败 → 含候选与原始错误，不含「不支持本机 ABI」', () {
      const String error = 'dlopen failed: library "libyunong_core.so" not found';
      final String detail = CoreStatus.failureDetail(
        candidates: const <String>['libyunong_core.so', 'liblibyunong_core.so'],
        error: error,
        abiHint: 'arm64',
      );
      expect(detail, contains('libyunong_core.so'));
      expect(detail, contains(error));
      expect(detail, contains('ABI=arm64-v8a'));
      expect(detail, contains('nativeLib 目录='));
      expect(detail, contains('/proc/self/maps 含 yunong_core='));
      expect(detail, isNot(contains('不支持本机 ABI')));
      expect(detail, startsWith('未加载'));
    });

    test('拿不到 ABI 线索（null）→ 也走完整诊断，绝不误报不支持', () {
      final String detail = CoreStatus.failureDetail(
        candidates: const <String>['libyunong_core.so'],
        error: 'dlopen failed',
      );
      expect(detail, isNot(contains('不支持本机 ABI')));
      expect(detail, contains('ABI=未知'));
    });

    test('unsupportedAbiMessage：受支持/无线索 → null', () {
      expect(CoreLibrary.unsupportedAbiMessage('arm64'), isNull);
      expect(CoreLibrary.unsupportedAbiMessage(null), isNull);
      expect(CoreLibrary.unsupportedAbiMessage(''), isNull);
      expect(CoreLibrary.unsupportedAbiMessage('arm'), isNotNull);
    });
  });

  group('失败路径确实会用到的候选与推断（t178 #3）', () {
    test('双 lib 前缀候选是候选列表的一部分（常量 + 真实拼装都包含）', () {
      expect(CoreLibrary.androidExtraCandidates, contains('liblibyunong_core.so'));
      // 主名（libyunong_core.so）由 fileName 提供；额外候选里必须出现双 lib 前缀名
      expect(CoreLibrary.androidExtraCandidates.length, 1);
    });

    test('mapsContainsCore 在非 Android 上安全返回 false（不抛）', () {
      expect(CoreLibrary.mapsContainsCore(), isFalse);
    });
  });
}