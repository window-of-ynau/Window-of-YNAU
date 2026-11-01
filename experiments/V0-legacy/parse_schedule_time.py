"""解析教务系统导出的 schedule.html，生成 schedule_time.json。

一条记录 = 一个“课程块”（同一门课、同一星期、同一周次和节次）。
课表 HTML 中，同一个课程块会按每个小节重复出现；这里会自动合并，
并且支持一个格子里用 ------ 分隔的多门课。
"""

import json
import re
from pathlib import Path

from bs4 import BeautifulSoup

BASE_DIR = Path(__file__).resolve().parent
HTML_FILE = BASE_DIR / "schedule.html"
JSON_FILE = BASE_DIR / "schedule_time.json"

BLOCK_SEPARATOR_RE = re.compile(r"(?:<br\s*/?>\s*)?-{5,}(?:\s*<br\s*/?>)*", re.I)
PERIOD_IN_TEXT_RE = re.compile(r"\[(.+?)\]")
WEEK_RANGE_RE = re.compile(r"(\d+)\s*[-~—至]\s*(\d+)")
ROW_PERIOD_RE = re.compile(r"第\s*(\d+)\s*节")


def clean_text(tag):
    """提取标签文字，去掉换行和 &nbsp;，但保留课程名中的普通空格。"""
    text = tag.get_text("", strip=True).replace("\xa0", "")
    return re.sub(r"\s+", " ", text).strip()


def split_cell_blocks(td):
    """按 ------ 分隔线，把一个 <td> 拆成多个课程块。

    课表每个格子有两份内容：kbcontent1 是简版，kbcontent 是带教师、
    周次(节次) 的完整版。只解析完整版，避免同一门课被解析两次。
    """
    cells = td.find_all("div", class_="kbcontent", recursive=False)
    if not cells:
        cells = [td]

    blocks = []
    for cell in cells:
        html = cell.decode_contents()
        parts = BLOCK_SEPARATOR_RE.split(html)
        blocks.extend(part for part in parts if "<font" in part.lower())
    return blocks


def parse_week_numbers(week_text):
    """把 '4-15(周)'、'1-5,7-9(周)'、'1-16单周' 转成周次列表。"""
    if not week_text:
        return []

    text = str(week_text).replace("（", "(").replace("）", ")")
    text = text.replace("(周)", "").replace("周", "").replace("第", "")
    odd_only = "单" in text
    even_only = "双" in text
    text = re.sub(r"[单双()]", "", text)

    weeks = set()
    for token in re.split(r"[,，、;；\s]+", text):
        if not token:
            continue
        match = WEEK_RANGE_RE.fullmatch(token)
        if match:
            start, end = map(int, match.groups())
            if start > end:
                start, end = end, start
            weeks.update(range(start, end + 1))
        else:
            weeks.update(int(n) for n in re.findall(r"\d+", token))

    if odd_only:
        weeks = {week for week in weeks if week % 2 == 1}
    elif even_only:
        weeks = {week for week in weeks if week % 2 == 0}
    return sorted(weeks)


def parse_period_range(period_text, fallback_period):
    """把 '01-02节' 或 '第11节' 转成 (开始节, 结束节)。"""
    numbers = [int(n) for n in re.findall(r"\d+", period_text or "")]
    if not numbers and fallback_period is not None:
        numbers = [fallback_period]
    if not numbers:
        return None
    return min(numbers), max(numbers)


def parse_block(block_html, weekday, fallback_period):
    """解析一个课程块（分隔线之间的一段 HTML）。"""
    fragment = BeautifulSoup(block_html, "html.parser")
    data = {}

    for font in fragment.find_all("font"):
        text = clean_text(font)
        if not text:
            continue
        title = (font.get("title") or "").strip()

        if title == "教师":
            data.setdefault("教师", text)
        elif title == "周次(节次)":
            period_match = PERIOD_IN_TEXT_RE.search(text)
            weeks_text = PERIOD_IN_TEXT_RE.sub("", text).strip()
            if period_match and "节次" not in data:
                data["节次"] = period_match.group(1).strip()
            if weeks_text and "周次" not in data:
                data["周次"] = weeks_text
        elif title == "教室":
            data.setdefault("教室", text)
        elif not title and "课程名" not in data:
            data["课程名"] = text

    if not data.get("课程名"):
        return None

    period_range = parse_period_range(data.get("节次"), fallback_period)
    if not period_range:
        return None
    start_period, end_period = period_range
    weeks = parse_week_numbers(data.get("周次"))

    record = {
        "课程名": data["课程名"],
        "周次": data.get("周次", ""),
        "教室": data.get("教室", ""),
        "教师": data.get("教师", ""),
        "节次": data.get("节次") or f"{start_period:02d}-{end_period:02d}节",
        "星期": weekday,
        "节": (
            f"第{start_period}-{end_period}节"
            if start_period != end_period
            else f"第{start_period}节"
        ),
        "开始节": start_period,
        "结束节": end_period,
        "周次列表": weeks,
    }
    if weeks:
        record["开始周"] = weeks[0]
        record["结束周"] = weeks[-1]
    return record


def main():
    with open(HTML_FILE, "r", encoding="utf-8") as file:
        soup = BeautifulSoup(file.read(), "html.parser")

    table = soup.find("table", id="timetable")
    if not table:
        raise SystemExit("没找到 id=timetable 的课表")

    rows = table.find_all("tr")
    header = next(
        (
            row
            for row in rows
            if any("星期" in th.get_text(strip=True) for th in row.find_all("th"))
        ),
        None,
    )
    if not header:
        raise SystemExit("没找到星期表头")

    weekdays = [
        th.get_text(strip=True)
        for th in header.find_all("th")
        if "星期" in th.get_text(strip=True)
    ]

    courses = []
    seen = set()

    for row in rows:
        th = row.find("th")
        if not th:
            continue
        period_match = ROW_PERIOD_RE.search(clean_text(th))
        if not period_match:
            continue
        row_period = int(period_match.group(1))

        for index, td in enumerate(row.find_all("td", recursive=False)):
            if index >= len(weekdays):
                break
            for block in split_cell_blocks(td):
                record = parse_block(block, weekdays[index], row_period)
                if not record:
                    continue

                # 同一个课程块会在每个小节重复出现，按完整信息去重
                key = (
                    record["课程名"],
                    record["星期"],
                    record["周次"],
                    record["节次"],
                    record["教室"],
                    record["教师"],
                )
                if key in seen:
                    continue
                seen.add(key)
                courses.append(record)

    print(f"共提取 {len(courses)} 个课程块")
    print(json.dumps(courses[:5], ensure_ascii=False, indent=2))

    with open(JSON_FILE, "w", encoding="utf-8") as file:
        json.dump(courses, file, ensure_ascii=False, indent=2)
    print(f"\n已保存到 {JSON_FILE.name}")


if __name__ == "__main__":
    main()
