#!/usr/bin/env python3
r"""markdown 体检：表格列数一致 + 相对 .md 链接存在（scripts/check_docs.py）。

用法：
    python scripts/check_docs.py docs
    python scripts/check_docs.py .        # 整个仓库（自动跳过 .git 等隐藏目录）

规则：
  1. 表格块（连续以 `|` 开头的行，至少 2 行）内，每行的 `|` 数量必须与块首行一致；
     `\|` 是转义竖线，不计数。围栏代码块（``` / ~~~）里的内容一律跳过。
  2. `](xxx.md)` 这种相对链接必须在文件系统上存在；http:// https:// // mailto:
     以及纯锚点 `#xxx` 跳过。链接里的 `#锚点`、`?查询串`、`%20` 会被剥掉后再查。
     以 `/` 开头的站点绝对路径不是相对链接，跳过。

只依赖标准库；按 UTF-8（含 BOM）读文件；Windows 路径与中文目录都能处理。
退出码：0 没问题；1 有问题；2 参数错误（目录不存在等）。
"""

import argparse
import sys
from pathlib import Path
from urllib.parse import unquote

FENCE_CHARS = ("`", "~")


def update_fence(line, fence):
    """返回新的围栏状态 (字符, 长度)；None 表示不在代码块内。"""
    stripped = line.lstrip()
    if not stripped:
        return fence
    char = stripped[0]
    if char not in FENCE_CHARS:
        return fence
    run = len(stripped) - len(stripped.lstrip(char))
    if fence is None:
        if run >= 3:
            return (char, run)
        return None
    fence_char, fence_len = fence
    if char == fence_char and run >= fence_len and stripped[run:].strip() == "":
        return None
    return fence


def count_pipes(line):
    """数没有反斜杠转义的 `|`。"""
    count = 0
    escaped = False
    for char in line:
        if char == "|" and not escaped:
            count += 1
        if char == "\\":
            escaped = not escaped
        else:
            escaped = False
    return count


def iter_link_targets(line):
    """产出 `](...)` 里的原始内容；允许目标里出现一层括号。"""
    index = 0
    while True:
        start = line.find("](", index)
        if start == -1:
            return
        pos = start + 2
        depth = 1
        while pos < len(line):
            char = line[pos]
            if char == "(":
                depth += 1
            elif char == ")":
                depth -= 1
                if depth == 0:
                    break
            pos += 1
        if pos >= len(line):
            return
        yield line[start + 2:pos]
        index = pos + 1


def link_target(raw):
    """从 `target` / `target "title"` / `<target>` 里取出纯目标。"""
    raw = raw.strip()
    if not raw:
        return None
    if raw.startswith("<"):
        end = raw.find(">")
        if end == -1:
            return None
        return raw[1:end].strip()
    parts = raw.split()
    return parts[0] if parts else None


def is_relative_md_link(target):
    if not target:
        return False
    lowered = target.lower()
    if lowered.startswith(("http://", "https://", "//", "mailto:", "tel:", "file:")):
        return False
    if target.startswith("#"):
        return False
    path_part = target.split("#", 1)[0].split("?", 1)[0]
    if not path_part or not path_part.lower().endswith(".md"):
        return False
    if path_part.startswith("/") or Path(path_part).is_absolute():
        # 站点绝对路径 / 盘符绝对路径，不属于"相对链接"
        return False
    return True


def display_path(path, root):
    try:
        return path.relative_to(root).as_posix()
    except ValueError:
        return path.as_posix()


def check_file(path, root):
    """返回 (problems, table_count, link_count)。problems 元素为 (位置, 说明)。"""
    shown = display_path(path, root)
    try:
        text = path.read_text(encoding="utf-8-sig")
    except UnicodeDecodeError as exc:
        return [(shown, 1, "not valid UTF-8: %s" % exc)], 0, 0
    except OSError as exc:
        return [(shown, 1, "cannot read file: %s" % exc)], 0, 0

    lines = text.splitlines()
    problems = []

    # 第一遍：标出围栏代码块（含围栏行本身），后面的检查全部跳过这些行。
    fenced = set()
    fence = None
    for index, line in enumerate(lines):
        new_fence = update_fence(line, fence)
        if fence is not None or new_fence is not None:
            fenced.add(index)
        fence = new_fence

    # 第二遍：链接。
    link_count = 0
    for index, line in enumerate(lines):
        if index in fenced:
            continue
        for raw in iter_link_targets(line):
            target = link_target(raw)
            if not is_relative_md_link(target):
                continue
            link_count += 1
            path_part = target.split("#", 1)[0].split("?", 1)[0]
            relative = unquote(path_part).replace("\\", "/")
            candidate = path.parent / relative
            try:
                exists = candidate.exists()
            except OSError:
                exists = False
            if not exists:
                problems.append((shown, index + 1, "link target not found: %s" % path_part))

    # 第三遍：表格块。
    table_count = 0
    index = 0
    while index < len(lines):
        line = lines[index]
        if index in fenced or not line.strip().startswith("|"):
            index += 1
            continue
        rows = []
        while (
            index < len(lines)
            and index not in fenced
            and lines[index].strip().startswith("|")
        ):
            rows.append((index + 1, lines[index]))
            index += 1
        if len(rows) < 2:
            continue
        table_count += 1
        header_pipes = count_pipes(rows[0][1])
        for lineno, row in rows[1:]:
            pipes = count_pipes(row)
            if pipes != header_pipes:
                problems.append(
                    (
                        shown,
                        lineno,
                        "table row has %d pipes, header has %d" % (pipes, header_pipes),
                    )
                )

    return problems, table_count, link_count


def iter_md_files(root):
    for path in sorted(root.rglob("*")):
        if not path.is_file() or path.suffix.lower() != ".md":
            continue
        try:
            relative = path.relative_to(root)
        except ValueError:
            relative = path
        if any(part.startswith(".") for part in relative.parts[:-1]):
            continue
        yield path


def main(argv=None):
    for stream in (sys.stdout, sys.stderr):
        try:
            stream.reconfigure(encoding="utf-8", errors="replace")
        except (AttributeError, ValueError, OSError):
            pass

    parser = argparse.ArgumentParser(
        description="校验 markdown 表格列数与相对 .md 链接（递归扫描目录）。"
    )
    parser.add_argument("directory", metavar="DIRECTORY", help="要扫描的目录，例如 docs 或 .")
    args = parser.parse_args(argv)

    root = Path(args.directory)
    if not root.is_dir():
        print("ERROR: not a directory: %s" % root, file=sys.stderr)
        return 2

    files = list(iter_md_files(root))
    all_problems = []
    table_total = 0
    link_total = 0
    for path in files:
        problems, tables, links = check_file(path, root)
        all_problems.extend(problems)
        table_total += tables
        link_total += links

    all_problems.sort(key=lambda item: (item[0], item[1]))
    for location, lineno, message in all_problems:
        print("%s:%d: %s" % (location, lineno, message))

    print(
        "checked %d file(s), %d table(s), %d link(s); %d problem(s)"
        % (len(files), table_total, link_total, len(all_problems))
    )
    return 1 if all_problems else 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except KeyboardInterrupt:
        print("interrupted", file=sys.stderr)
        sys.exit(130)
