"""聊天记忆、用户画像与文档状态的 JSON 持久化。"""

import json
import os
import threading

from dotenv import load_dotenv

load_dotenv()

BASE_DIR = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def data_path(env_name, default):
    """读取 .env 中的数据文件路径，相对路径统一落到项目根目录。"""
    path = os.getenv(env_name, default)
    return path if os.path.isabs(path) else os.path.join(BASE_DIR, path)


MEMORY_FILE = data_path("MEMORY_FILE", "memory.json")
PROFILE_FILE = data_path("PROFILE_FILE", "profile.json")
FILE_STATE_FILE = data_path("FILE_STATE_FILE", "file_state.json")

PROFILE_LOCK = threading.RLock()
FILE_LOCK = threading.RLock()
MEMORY_LOCK = threading.RLock()

# 每次「新对话」自增；正在生成的回答若跨越了清空动作，就不再写回记忆
_memory_epoch = 0

AUTO_PROFILE_FIELDS = ("student_union_role", "class_role", "current_activities", "other_facts")


def _read_json(path, default):
    if not os.path.exists(path):
        return default
    try:
        with open(path, "r", encoding="utf-8") as f:
            return json.load(f)
    except Exception as e:
        print(f"读取 {os.path.basename(path)} 失败:", e)
        return default


def _write_json(path, data):
    os.makedirs(os.path.dirname(path) or ".", exist_ok=True)
    tmp = f"{path}.tmp"
    with open(tmp, "w", encoding="utf-8") as f:
        json.dump(data, f, ensure_ascii=False, indent=2)
    os.replace(tmp, path)


# ================= 聊天记忆 =================

def load_memory():
    data = _read_json(MEMORY_FILE, [])
    return data if isinstance(data, list) else []


def save_memory(mem):
    with MEMORY_LOCK:
        _write_json(MEMORY_FILE, mem)


def clear_memory():
    """清空聊天记忆，并让仍在生成中的回答失效。"""
    global _memory_epoch
    with MEMORY_LOCK:
        _memory_epoch += 1
        _write_json(MEMORY_FILE, [])


def memory_epoch():
    with MEMORY_LOCK:
        return _memory_epoch


# ================= 用户画像 =================

def load_profile():
    data = _read_json(PROFILE_FILE, {})
    return data if isinstance(data, dict) else {}


def save_profile(profile):
    with PROFILE_LOCK:
        _write_json(PROFILE_FILE, profile)


def build_system_prompt():
    p = load_profile()
    lines = ["你是「云农之窗」，一个专为云南农业大学学生打造的智能校园助理。回答要简洁高效。"
             "【重要指令】除非用户明确要求你称呼他，否则在任何回答中，绝对不要带上用户的名字。不要用‘某某你好’作为开头，直接回答问题即可。"]
    if p.get("name"): lines.append(f"用户的名字是 {p['name']}（仅供你知道，不要用于称呼）。")
    if p.get("school"): lines.append(f"用户在 {p['school']} 就读。")
    if p.get("major"): lines.append(f"用户的专业是 {p['major']}。")
    if p.get("preferences"): lines.append(f"用户的偏好与长期画像：{p['preferences']}")
    if p.get("student_union_role"): lines.append(f"用户的学生会/团委等组织职务：{p['student_union_role']}。")
    if p.get("class_role"): lines.append(f"用户的班级职务：{p['class_role']}。")
    if p.get("current_activities"):
        acts = "；".join(str(x).strip() for x in p["current_activities"] if str(x).strip())
        if acts: lines.append(f"用户最近正在做的事：{acts}。")
    if p.get("other_facts"):
        facts = "；".join(str(x).strip() for x in p["other_facts"] if str(x).strip())
        if facts: lines.append(f"其他关于用户的长期信息：{facts}。")
    if p.get("schedule_notes"): lines.append(f"用户的特殊日程备注：{p['schedule_notes']}")
    lines.append("以上是长期记住的用户信息，请在相关时自然地结合使用，不要无端罗列或反复提及，也不要臆测未提供的信息。")
    return "\n".join(lines)


def merge_profile(data):
    """把抽取出的画像字段合并进 profile.json，返回 (profile, changed)。"""
    with PROFILE_LOCK:
        profile = load_profile()
        changed = False

        for key in ("student_union_role", "class_role"):
            val = data.get(key)
            if isinstance(val, str) and val.strip() and profile.get(key) != val.strip():
                profile[key] = val.strip()
                changed = True

        for key in ("current_activities", "other_facts"):
            items = data.get(key)
            if not isinstance(items, list):
                continue
            cleaned = [str(x).strip() for x in items if str(x).strip()]
            if not cleaned:
                continue
            existing = profile.get(key) if isinstance(profile.get(key), list) else []
            merged = list(dict.fromkeys(existing + cleaned))[-12:]
            if merged != existing:
                profile[key] = merged
                changed = True

        if changed:
            save_profile(profile)
        return profile, changed


def delete_profile_field(field, value=""):
    with PROFILE_LOCK:
        profile = load_profile()
        if field in profile:
            if value and isinstance(profile[field], list):
                profile[field] = [x for x in profile[field] if str(x) != value]
                if not profile[field]:
                    del profile[field]
            else:
                del profile[field]
            save_profile(profile)
        return {"ok": True, "profile": profile}


def reset_profile():
    with PROFILE_LOCK:
        profile = load_profile()
        for key in AUTO_PROFILE_FIELDS:
            profile.pop(key, None)
        save_profile(profile)
        return profile


# ================= 文档状态 =================

def load_file_state():
    data = _read_json(FILE_STATE_FILE, {})
    return data if isinstance(data, dict) else {}


def save_file_state(state):
    with FILE_LOCK:
        _write_json(FILE_STATE_FILE, state)
