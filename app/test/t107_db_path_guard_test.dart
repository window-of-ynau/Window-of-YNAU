// t107：db_path 语义守护（防止有人把“数据库文件”和“凭据目录”统一成一个值）。
//
// 背景（实测）：`db_path` 是**数据库文件**路径；传目录会得到
//   `SQLITE_CANTOPEN: unable to open database file: <目录>`
// 而 `data_dir`（凭据/会话目录）是**目录**。两者语义不同，混用会让工具或凭据其一失效。

import 'dart:convert';
import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/src/core/app_paths.dart';
import 'package:yunong/src/core/chat_service.dart';

void main() {
  group('db_path 语义守护', () {
    final String dbFile = '${Directory.systemTemp.path}/yunong/app.db';
    final String dataDir = AppPaths.dataDirOf(dbFile);

    ChatRequestDto request({String? dbPath, String? dataDir}) => ChatRequestDto(
          channel: ChatChannel.byok,
          model: 'deepseek-chat',
          messages: <ChatTurn>[ChatTurn.user('hi')],
          dbPath: dbPath,
          dataDir: dataDir,
        );

    test('断言 1：db_path 以 .db 结尾（不是目录）', () {
      final Map<String, dynamic> json =
          jsonDecodeMap(request(dbPath: dbFile, dataDir: dataDir).toJson());
      expect(json['db_path'], endsWith('.db'));
    });

    test('断言 2：db_path 不等于缓存/凭据目录（防止传 _dataDir）', () {
      final Map<String, dynamic> json =
          jsonDecodeMap(request(dbPath: dbFile, dataDir: dataDir).toJson());
      expect(json['db_path'], isNot(dataDir));
      expect(json['db_path'], isNot(endsWith('/')));
    });

    test('断言 3：data_dir 与 db_path 是两个不同的值', () {
      final Map<String, dynamic> json =
          jsonDecodeMap(request(dbPath: dbFile, dataDir: dataDir).toJson());
      expect(json['data_dir'], dataDir, reason: '凭据用目录');
      expect(json['db_path'], dbFile, reason: '工具用数据库文件');
      expect(json['data_dir'], isNot(json['db_path']),
          reason: '两者混用会让“凭据目录”被当库文件或反之，工具/凭据其一必坏');
    });
  });
}

/// 极简 JSON 解析（测试里直接用的标准解码器）。
Map<String, dynamic> jsonDecodeMap(String raw) =>
    jsonDecode(raw) as Map<String, dynamic>;
