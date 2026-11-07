// t159：应用图标接线护栏 —— 不是 Flutter 默认图标、各密度尺寸正确、自适应图标结构完整。

import 'dart:io';
import 'dart:typed_data';

import 'package:flutter_test/flutter_test.dart';

const String _resAndroid = 'android/app/src/main/res';
const String _winIco = 'windows/runner/resources/app_icon.ico';

/// 读 PNG 的 IHDR 宽高（不需要图片解码库：PNG 头固定 8 字节签名 + 4 长度 + 'IHDR' + w/h）。
({int width, int height}) pngSize(File file) {
  final Uint8List bytes = file.readAsBytesSync();
  expect(bytes.length, greaterThan(24), reason: '${file.path} 太小，不像是合法 PNG');
  expect(bytes[0], 0x89);
  expect(bytes[1], 0x50); // 'P'
  expect(bytes[2], 0x4E); // 'N'
  expect(bytes[3], 0x47); // 'G'
  final ByteData data = ByteData.sublistView(bytes);
  return (width: data.getUint32(16), height: data.getUint32(20));
}

void main() {
  group('Windows 图标（t159）', () {
    test('app_icon.ico 已替换为设计稿（不是 Flutter 默认图标）', () {
      final File ico = File(_winIco);
      expect(ico.existsSync(), isTrue);

      final int size = ico.lengthSync();
      // Flutter 模板自带的 app_icon.ico 约 33 KB；设计稿 ICO（16/24/32/48/64/128/256 内嵌）约 12.7 KB。
      expect(size, lessThan(20 * 1024), reason: '仍像 Flutter 默认图标（约 33KB）：$size bytes');
      expect(size, greaterThan(4 * 1024), reason: '太小，可能不是多尺寸 ICO：$size bytes');

      // ICO 头：保留位 0/0 + type=1 + count
      final Uint8List header = ico.readAsBytesSync().sublist(0, 6);
      expect(header[0], 0);
      expect(header[1], 0);
      expect(header[2], 1, reason: 'ICO type 必须是 1');
      expect(header[3], 0);
      final int count = header[4] | (header[5] << 8);
      expect(count, greaterThanOrEqualTo(5), reason: '多尺寸 ICO 的条目数：$count');
    });

    test('Runner.rc 仍引用该路径（替换而非改配置）', () {
      final String rc = File('windows/runner/Runner.rc').readAsStringSync();
      expect(rc.contains('app_icon.ico'), isTrue);
    });
  });

  group('Android 图标（t159）', () {
    const Map<String, int> expected = <String, int>{
      'mipmap-mdpi': 48,
      'mipmap-hdpi': 72,
      'mipmap-xhdpi': 96,
      'mipmap-xxhdpi': 144,
      'mipmap-xxxhdpi': 192,
    };

    for (final MapEntry<String, int> entry in expected.entries) {
      test('${entry.key} = ${entry.value}×${entry.value}', () {
        final File png =
            File('$_resAndroid/${entry.key}/ic_launcher.png');
        expect(png.existsSync(), isTrue, reason: '缺少 ${png.path}');
        final ({int width, int height}) size = pngSize(png);
        expect(size.width, entry.value);
        expect(size.height, entry.value);
      });
    }

    test('自适应图标：anydpi-v26 XML + 前景/背景 drawable 齐全', () {
      final File xml = File('$_resAndroid/mipmap-anydpi-v26/ic_launcher.xml');
      expect(xml.existsSync(), isTrue);
      final String body = xml.readAsStringSync();
      expect(body, contains('<adaptive-icon'));
      expect(body, contains('@drawable/ic_launcher_background'));
      expect(body, contains('@drawable/ic_launcher_foreground'));

      final File foreground = File('$_resAndroid/drawable-nodpi/ic_launcher_foreground.png');
      final File background = File('$_resAndroid/drawable-nodpi/ic_launcher_background.png');
      expect(foreground.existsSync(), isTrue);
      expect(background.existsSync(), isTrue);
      // 两层都是 432 画布（不随密度缩放）
      expect(pngSize(foreground).width, 432);
      expect(pngSize(background).width, 432);

      // 圆形图标复用同一套层
      final File round = File('$_resAndroid/mipmap-anydpi-v26/ic_launcher_round.xml');
      expect(round.existsSync(), isTrue);
      expect(round.readAsStringSync(), contains('<adaptive-icon'));
    });

    test('AndroidManifest 的 android:icon 指向 @mipmap/ic_launcher', () {
      final String manifest = File('android/app/src/main/AndroidManifest.xml').readAsStringSync();
      expect(manifest, contains('android:icon="@mipmap/ic_launcher"'));
    });
  });
}


