"""AI 能力封装：DeepSeek 对话、千问识图、联网搜索与文档问答。"""

import base64
import json
import os
import re
import time
from datetime import datetime

import requests
from dotenv import load_dotenv
from openai import OpenAI

from . import campus_api, memory_service

load_dotenv()


def _env(name, default):
    return os.getenv(name) or default


def _env_int(name, default):
    try:
        return int(os.getenv(name, str(default)))
    except ValueError:
        return default


# 以下配置均可用 .env 覆盖；API Key 只从环境变量读取
DEEPSEEK_BASE_URL = _env("DEEPSEEK_BASE_URL", "https://api.deepseek.com")
DEEPSEEK_MODEL = _env("DEEPSEEK_MODEL", "deepseek-chat")
DEEPSEEK_SEARCH_URL = _env("DEEPSEEK_SEARCH_URL", "https://api.deepseek.com/v1/tools/search")
DASHSCOPE_BASE_URL = _env("DASHSCOPE_BASE_URL", "https://dashscope.aliyuncs.com/compatible-mode/v1")
QWEN_LONG_MODEL = _env("QWEN_LONG_MODEL", "qwen-long")
VISION_BASE_URL = _env(
    "VISION_BASE_URL",
    "https://ws-pqr8hfihzt08v2zw.cn-beijing.maas.aliyuncs.com/compatible-mode/v1",
)
VISION_MODEL = _env("VISION_MODEL", "qwen3.7-plus")

MAX_UPLOAD_MB = _env_int("MAX_UPLOAD_MB", 50)
TEMP_DIR = memory_service.data_path("TEMP_DIR", "temp")
SEARCH_KEYWORDS = ["搜", "查一下", "最新", "新闻", "天气", "大创", "比赛"]


def _api_key(name):
    key = os.getenv(name)
    if not key:
        raise RuntimeError(f"未配置 {name}")
    return key


def get_deepseek_client():
    return OpenAI(api_key=_api_key("DEEPSEEK_API_KEY"), base_url=DEEPSEEK_BASE_URL)


def get_dashscope_client():
    return OpenAI(api_key=_api_key("DASHSCOPE_API_KEY"), base_url=DASHSCOPE_BASE_URL)


def get_vision_client():
    return OpenAI(api_key=_api_key("DASHSCOPE_API_KEY"), base_url=VISION_BASE_URL)


# ================= DeepSeek 对话 =================

def ask_deepseek(messages, timeout=30, **options):
    """调用 DeepSeek 对话接口，返回文本内容。"""
    resp = get_deepseek_client().chat.completions.create(
        model=DEEPSEEK_MODEL,
        messages=messages,
        timeout=timeout,
        **options,
    )
    return resp.choices[0].message.content


def describe_image(content, mime="image/jpeg"):
    """千问视觉识图，返回文本结果（失败时返回提示文本）。"""
    try:
        b64 = base64.b64encode(content).decode()
        resp = get_vision_client().chat.completions.create(
            model=VISION_MODEL,
            messages=[
                {"role": "system", "content": memory_service.build_system_prompt()},
                {"role": "user", "content": [
                    {"type": "text", "text": "请仔细阅读这张图片，提取图片中的关键信息（如果是课表请整理成表格），并用自然的中文回复我。"},
                    {"type": "image_url", "image_url": {"url": f"data:{mime};base64,{b64}"}},
                ]},
            ],
            timeout=60,
        )
        return resp.choices[0].message.content
    except Exception as e:
        return f"识图失败：{e}"


# ================= 联网搜索 =================

def web_search(query):
    """调用 DeepSeek 搜索接口，返回拼好的搜索结果文本。"""
    try:
        api_key = _api_key("DEEPSEEK_API_KEY")
        resp = requests.post(
            DEEPSEEK_SEARCH_URL,
            headers={"Authorization": f"Bearer {api_key}", "Content-Type": "application/json"},
            json={"query": query[:128], "count": 5, "need_content": False},
            timeout=8,
        )
        resp.raise_for_status()
        results = resp.json().get("results", [])
        if not results:
            return "（搜索无结果）"
        text = "\n【联网搜索结果】\n"
        for i, item in enumerate(results, 1):
            text += f"{i}. {item.get('title', '无标题')}\n摘要：{item.get('summary', '')}\n\n"
        return text
    except Exception as e:
        print("搜索失败:", e)
        return "（搜索超时，请直接回答用户问题）"


# ================= 用户画像自动抽取 =================

PROFILE_HINTS = [
    "我是", "我叫", "我担任", "我当", "我负责", "我最近", "我在做", "我在准备",
    "我正在", "班长", "团支书", "学委", "学习委员", "宣传委员", "组织委员",
    "部长", "副部长", "主席", "副主席", "干事", "社长", "委员", "学生会",
    "团委", "社团", "部门", "正在", "筹备", "准备", "这学期", "目前",
    "我们班", "我们部门", "我们学院", "评优", "迎新", "项目",
    "文艺部", "体育部", "宣传部", "组织部", "学习部", "生活部", "纪检部",
    "外联部", "实践部", "新媒体", "办公室", "秘书处", "志愿者", "青协",
]

PROFILE_EXTRACT_SYSTEM = """你是「云农之窗」的用户画像抽取器。请从用户的一条发言中，抽取关于用户本人的长期特征，只输出一个 JSON 对象（不要输出任何解释，不要使用 Markdown 代码块）。

JSON 结构：
{
  "student_union_role": "用户的学生会/团委/社团等组织职务，例如：学生会文艺部部长；没有明确提到则为 null",
  "class_role": "用户的班级职务，例如：班长、团支书、学习委员；没有明确提到则为 null",
  "current_activities": ["用户最近或目前正在做的事、正在筹备的活动、正在准备的目标；没有则为空数组"],
  "other_facts": ["其他值得长期记住的个人信息，例如专业方向、兴趣爱好、升学/就业目标、常驻校区；没有则为空数组"]
}

规则：
1. 只提取用户明确说出的、关于用户本人的事实，绝不猜测。
2. 不提取一次性问题、临时情绪、与他人的关系。
3. 职务保留用户原话中的具体组织与级别。
4. 没有任何可提取的信息时，返回 {"student_union_role": null, "class_role": null, "current_activities": [], "other_facts": []}。"""


def _parse_json_object(text):
    """容错解析模型返回的 JSON（可能带 ```json 代码块）。"""
    text = (text or "").strip()
    if text.startswith("```"):
        parts = text.split("```")
        text = parts[1] if len(parts) > 1 else text
        if text.lstrip().lower().startswith("json"):
            text = text.lstrip()[4:]
    try:
        data = json.loads(text.strip())
        return data if isinstance(data, dict) else {}
    except Exception:
        return {}


def extract_profile(user_message):
    """调用 DeepSeek 抽取用户长期特征，失败返回空字典。"""
    try:
        content = ask_deepseek(
            [
                {"role": "system", "content": PROFILE_EXTRACT_SYSTEM},
                {"role": "user", "content": user_message[:2000]},
            ],
            timeout=20,
            temperature=0,
        )
        return _parse_json_object(content)
    except Exception as e:
        print("画像抽取失败:", e)
        return {}


def update_profile_from_message(user_message):
    """从用户发言中抽取特征并合并进 profile.json（后台任务）。"""
    if not user_message or not any(hint in user_message for hint in PROFILE_HINTS):
        return
    data = extract_profile(user_message)
    if not data:
        return
    profile, changed = memory_service.merge_profile(data)
    if changed:
        print("=== 已更新用户画像 ===", json.dumps(profile, ensure_ascii=False))


# ================= qwen-long 文档问答 =================

def cleanup_temp_files(max_age_seconds=3600):
    """清理 temp/ 中超过指定时间的残留文件。"""
    if not os.path.isdir(TEMP_DIR):
        return 0
    now = time.time()
    removed = 0
    for name in os.listdir(TEMP_DIR):
        path = os.path.join(TEMP_DIR, name)
        try:
            if os.path.isfile(path) and now - os.path.getmtime(path) > max_age_seconds:
                os.remove(path)
                removed += 1
        except Exception as e:
            print("清理临时文件失败:", path, e)
    return removed


def upload_file_to_dashscope(filename, content, content_type=None):
    """通过 DashScope 兼容文件接口上传，返回 (file_id, name)。"""
    dc = get_dashscope_client()
    file_part = (filename, content, content_type) if content_type else (filename, content)
    file_obj = dc.files.create(file=file_part, purpose="file-extract")
    file_id = file_obj.id

    # 等待文件解析完成（通常几秒内；status 为 processed 才可用）
    status = getattr(file_obj, "status", None)
    for _ in range(15):
        if status in (None, "processed"):
            break
        time.sleep(1)
        status = getattr(dc.files.retrieve(file_id), "status", None)

    return file_id, (getattr(file_obj, "filename", None) or filename)


def build_file_context_messages(state):
    """构造 qwen-long 的 fileid 引用消息。"""
    file_id = state.get("file_id")
    name = state.get("name") or "上传的文件"
    return [
        {"role": "system", "content": f"用户上传了文件《{name}》，请结合该文件内容回答用户问题。"},
        {"role": "system", "content": f"fileid://{file_id}"},
    ]


def ask_qwen_long_with_file(state, question, memory=None):
    """基于已上传文件，用 qwen-long 回答一个问题。"""
    messages = [{"role": "system", "content": memory_service.build_system_prompt()}]
    messages += build_file_context_messages(state)
    if memory:
        messages += memory[-20:]
    messages.append({"role": "user", "content": question})

    resp = get_dashscope_client().chat.completions.create(
        model=QWEN_LONG_MODEL,
        messages=messages,
        timeout=120,
    )
    return resp.choices[0].message.content


def handle_document_upload(filename, content, content_type=None, question=""):
    """校验并上传文档到百炼，可按需直接回答附带的问题。"""
    if len(content) > MAX_UPLOAD_MB * 1024 * 1024:
        return {"ok": False, "error": f"文件太大，最大支持 {MAX_UPLOAD_MB}MB"}

    cleanup_temp_files()
    safe_name = re.sub(
        r"[^\w.\-()\u4e00-\u9fa5]", "_", os.path.basename(filename or "upload")
    ) or "upload"

    try:
        file_id, name = upload_file_to_dashscope(safe_name, content, content_type)
    except Exception as e:
        print("DashScope 文件上传失败:", e)
        return {"ok": False, "error": f"上传到百炼失败：{e}"}

    state = {
        "file_id": file_id,
        "name": name,
        "size": len(content),
        "uploaded_at": datetime.now().strftime("%Y-%m-%d %H:%M:%S"),
    }
    memory_service.save_file_state(state)

    result = {"ok": True, "file_id": file_id, "name": name, "message": "文件已上传，可以直接提问"}
    if question.strip():
        try:
            result["reply"] = ask_qwen_long_with_file(state, question.strip())
        except Exception as e:
            result["reply"] = f"文件已上传，但回答失败：{e}"
    return result


# ================= 对话总入口 =================

def chat_reply(message):
    """组装记忆、课表、搜索与文档上下文，返回助手回复。"""
    try:
        epoch = memory_service.memory_epoch()
        memory = memory_service.load_memory()
        file_state = memory_service.load_file_state()
        file_active = bool(file_state.get("file_id"))

        # 拦截跳转意图（有文档时跳过，避免把文档里的“成绩/教务”误判为跳转）
        if not file_active:
            jump_url = campus_api.detect_jump_target(message)
            if jump_url:
                return f"收到！请点击下方按钮直接前往：\n\n[🚀 点击前往系统]({jump_url})"

        # 拦截联网搜索意图（有文档时跳过，专注文档问答）
        user_message = message
        if not file_active and any(kw in message for kw in SEARCH_KEYWORDS):
            print("=== 触发搜索 ===")
            user_message = f"{web_search(message)}\n\n用户问题：{message}"

        # 组装消息：有文档时把 fileid 一起带上
        messages = [{
            "role": "system",
            "content": memory_service.build_system_prompt() + campus_api.build_schedule_context(),
        }]
        if file_active:
            messages += build_file_context_messages(file_state)
        messages += memory[-20:]
        messages.append({"role": "user", "content": user_message})

        # 有文档走 qwen-long，否则走 DeepSeek
        if file_active:
            response = get_dashscope_client().chat.completions.create(
                model=QWEN_LONG_MODEL,
                messages=messages,
                timeout=120,
            )
        else:
            response = get_deepseek_client().chat.completions.create(
                model=DEEPSEEK_MODEL,
                messages=messages,
                timeout=30,
            )
        reply = response.choices[0].message.content

        memory.append({"role": "user", "content": user_message})
        memory.append({"role": "assistant", "content": reply})
        # 生成期间用户点了「新对话」时，丢弃这次结果，避免记忆被重新写回
        if epoch == memory_service.memory_epoch():
            memory_service.save_memory(memory)
        return reply
    except Exception as e:
        print("=== 崩溃啦 ===", str(e))
        return f"系统开小差了：{str(e)}。你可以直接问我其他问题。"
