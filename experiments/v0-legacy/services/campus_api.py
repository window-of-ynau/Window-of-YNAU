"""教务系统跳转路由与本地课表读取。"""

import json
import os
import re
from datetime import datetime

from dotenv import load_dotenv
from fastapi import APIRouter
from fastapi.responses import RedirectResponse

from .memory_service import data_path

load_dotenv()

router = APIRouter()

# 快捷菜单配置
CAMPUS_MENU = {
    "portal": "https://jwxt.ynau.edu.cn/DefaultStu",
    "jwxt": "https://qzjwxt.ynau.edu.cn/jsxsd/framework/xsMainV.htmlx",
    "score": "https://jwgl.ynau.edu.cn/STU/cj_default.aspx",
    "table": "https://ldm.ynau.edu.cn/dashboard#/",
    "library": "http://10.8.225.101:8013/#/Home",
    "software": "http://software.ynau.edu.cn/",
}

SCHEDULE_FILE = data_path("SCHEDULE_FILE", "schedule.json")
SCHEDULE_TIME_FILE = data_path("SCHEDULE_TIME_FILE", "schedule_time.json")

# 第 1 周从哪天开始。可用 .env 的 SEMESTER_START 覆盖，日期格式 YYYY-MM-DD。
SEMESTER_START_DEFAULT = "2026-08-31"

# 节次换算规则：课表用的是全天连续编号（1-5 上午 / 6-10 下午 / 11-13 晚上），
# 而学生口语常按半天重新数（“下午三四节”= 第8-9节），必须写进提示词，
# 否则模型会把“下午第3-4节”当成上午的第3-4节，或反问“这是上午还是下午”。
PERIOD_RULES = """
【节次换算规则（必须遵守）】
学校的课表用“全天连续编号”：上午第1-5节、下午第6-10节、晚上第11-13节。
学生口语常按半天重新数，两者对应关系固定为：
- 下午第1~5节 = 全天第6~10节（下午第1节=第6节，第2节=第7节，第3节=第8节，第4节=第9节，第5节=第10节）。
- 上午第N节 = 全天第N节；晚上第N节 = 全天第(N+10)节（晚上第1节=第11节）。
- 听到“下午三四节”“周三下午一、二节”“下午第3-5节”等说法，先按 +5 换算成全天节次，再和课表比对；不要反问用户“是上午还是下午”，更不要把它当成上午的第3-4节。
- 用户只说“第X节”、没说明上下午时，按全天编号理解（1-5=上午，6-10=下午，11-13=晚上）。
- 判断是否与上课冲突，一律用换算后的全天节次；回答时写成“下午第X-Y节（第M-N节）”的双写法。
- 历史对话里曾把“周三下午三四节”错算成第3-4节，那是错误理解，以本规则为准。
"""


@router.get("/go/{action}")
async def go_to_campus(action: str):
    url = resolve_action(action)
    if url:
        return RedirectResponse(url=url)
    return {"error": f"没有找到 {action} 对应的系统"}


def resolve_action(action):
    """把菜单名解析成教务系统地址，未找到返回 None。"""
    return CAMPUS_MENU.get(action)


def detect_jump_target(message):
    """识别聊天中的跳转意图，返回要打开的系统地址。"""
    if "集成" in message or "教务" in message:
        return CAMPUS_MENU.get("portal")
    if "成绩" in message:
        return CAMPUS_MENU.get("score")
    return None


def _load_json(path, default):
    if not os.path.exists(path):
        return default
    try:
        with open(path, "r", encoding="utf-8") as f:
            data = json.load(f)
        return data
    except Exception as e:
        print(f"读取 {os.path.basename(path)} 失败，跳过:", e)
        return default


def load_schedule():
    """读取课程列表（schedule.json），失败时返回空列表。"""
    data = _load_json(SCHEDULE_FILE, [])
    return data if isinstance(data, list) else []


def load_schedule_times():
    """读取带上课时间的课表（schedule_time.json），失败时返回空列表。"""
    data = _load_json(SCHEDULE_TIME_FILE, [])
    return data if isinstance(data, list) else []


SCHEDULE_RULES = """
【课表与提醒规则（必须遵守）】
1. “周次”表示这门课实际开课的周。判断今天/明天/本周有没有课、是否需要提醒，必须先核对当前教学周是否在周次内。
2. 只有标记为“[本周]”的课程才算本周有课；标记为“[非本周]”的课程不得列入今日/明日课程，也不得提醒用户去上课。
3. 用户问某门课第几周上、周次、什么时候结课时，直接根据“周次”回答，不要回答“查不到课程周次”。
4. 用户问下周、第14周等其它周次时，按对应周次重新筛选，不要套用本周结果。
5. 回答“今天有什么课”时，只使用【今天实际上课的课程】；该列表为空就是今天没课，不要只凭星期几自行推断。
6. 历史对话里的课程安排可能已经过时或算错，一律以本段课表为准。
"""


def semester_start_date():
    """返回第 1 周的日期；配置错误时回退到默认值。"""
    raw = os.getenv("SEMESTER_START", SEMESTER_START_DEFAULT).strip()
    try:
        return datetime.strptime(raw, "%Y-%m-%d").date()
    except ValueError:
        print(f"SEMESTER_START 格式不正确：{raw!r}，使用默认值 {SEMESTER_START_DEFAULT}")
        return datetime.strptime(SEMESTER_START_DEFAULT, "%Y-%m-%d").date()


def current_teaching_week(now=None):
    """按 SEMESTER_START 计算当前教学周；开学前返回 0 或负数。"""
    now = now or datetime.now()
    return (now.date() - semester_start_date()).days // 7 + 1


def _week_numbers(item):
    """优先使用解析器生成的周次列表，旧 JSON 则回退解析“4-15(周)”。"""
    weeks = item.get("周次列表")
    if isinstance(weeks, list) and weeks:
        return {int(week) for week in weeks if str(week).isdigit()}

    text = str(item.get("周次") or "").replace("（", "(").replace("）", ")")
    text = text.replace("(周)", "").replace("周", "").replace("第", "")
    text = re.sub(r"[单双()]", "", text)
    numbers = set()
    for token in re.split(r"[,，、;；\s]+", text):
        match = re.fullmatch(r"(\d+)\s*[-~—至]\s*(\d+)", token)
        if match:
            start, end = map(int, match.groups())
            numbers.update(range(min(start, end), max(start, end) + 1))
        else:
            numbers.update(int(n) for n in re.findall(r"\d+", token))
    return numbers


def is_course_active(item, week):
    """判断一门课在当前教学周是否实际上课。"""
    return week >= 1 and week in _week_numbers(item)


def _course_line(item, active):
    mark = "本周" if active else "非本周"
    parts = [
        f"[{mark}] {item.get('课程名', '?')}",
        f"{item.get('星期', '?')} {item.get('节', '?')}",
        f"周次 {item.get('周次') or '未知'}",
        item.get("教室") or "教室未知",
    ]
    if item.get("教师"):
        parts.append(str(item["教师"]))
    return "- " + "｜".join(parts) + "\n"


def _sort_courses(courses, weekdays):
    order = {name: index for index, name in enumerate(weekdays)}
    return sorted(
        courses,
        key=lambda item: (
            order.get(item.get("星期"), 99),
            int(item.get("开始节") or 99),
        ),
    )


def build_schedule_context():
    """生成注入系统提示词的时间、当前周次与课表文本。"""
    now = datetime.now()
    weekdays = ["星期一", "星期二", "星期三", "星期四", "星期五", "星期六", "星期日"]
    today_str = now.strftime("%Y-%m-%d") + f"（{weekdays[now.weekday()]}）"
    week = current_teaching_week(now)
    start = semester_start_date()
    week_label = f"第{week}周" if week >= 1 else "尚未开学"

    text = f"\n\n【系统当前时间】今天是：{today_str}\n"
    text += f"【当前教学周】{week_label}（第1周从 {start:%Y-%m-%d} 开始）\n"
    text += PERIOD_RULES
    text += SCHEDULE_RULES

    times = load_schedule_times()
    if not times:
        text += "\n【用户本学期课程与上课时间】\n（没有读取到 schedule_time.json）\n"
        return text

    active = _sort_courses([item for item in times if is_course_active(item, week)], weekdays)
    inactive = _sort_courses([item for item in times if not is_course_active(item, week)], weekdays)

    today_name = weekdays[now.weekday()]
    today_courses = [item for item in active if item.get("星期") == today_name]
    text += f"\n【今天实际上课的课程】（已按第{week}周筛选）\n"
    if today_courses:
        text += "".join(_course_line(item, True) for item in today_courses)
    else:
        text += "（今天没有课）\n"

    text += "\n【本周课程】（当前教学周内实际上课）\n"
    if active:
        text += "".join(_course_line(item, True) for item in active)
    else:
        text += "（本周没有课）\n"

    if inactive:
        text += "\n【非本周课程】（只能用于查询周次，禁止作为近期上课提醒）\n"
        text += "".join(_course_line(item, False) for item in inactive)

    return text


@router.get("/api/schedule")
async def get_schedule():
    """给前端查询课表：返回当前教学周以及每门课是否在本周上课。"""
    week = current_teaching_week()
    courses = []
    for item in load_schedule_times():
        course = dict(item)
        course["本周"] = is_course_active(course, week)
        courses.append(course)
    return {"current_week": week, "courses": courses}
