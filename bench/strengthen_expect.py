"""t162：把 expect.contains 换成"答案里有、问题里没有"的事实词。

动机：原来的期望词（如“周三”“课”）几乎都出现在**问题**里，于是"回显问题"也能命中 ⇒
命中率指标形同虚设（实测 96% 命中率对应的是 mock 完全没取到答案）。
现在每条用例至少有一个**只可能来自答案**的事实词（时间/地点/数值/专有短语），
命中率才真的衡量"答案内容有没有被完整送达"。
"""

import json
import re
from pathlib import Path

CASES = Path("bench/cases/full.jsonl")

TIME = re.compile(r"\d{1,2}:\d{2}(?:-\d{1,2}:\d{2})?")
ROOM = re.compile(r"[A-Z]\d{3}")
NUMBER_UNIT = re.compile(r"\d+\s*(?:学分|元|年|天|小时|周|门|节|次|分|页|层|公里)")
PHRASE_SPLIT = re.compile(r"[\s，。；：、（）()“”\"'!？?、,;:]+")


def pick_fact(answer: str, question: str) -> str:
    """从答案里挑一个"问题里没有"的事实词；挑不到就返回空串（调用方报错，不静默放过）。"""
    for pattern in (TIME, ROOM, NUMBER_UNIT):
        for match in pattern.finditer(answer):
            term = match.group(0).strip()
            if term and term not in question:
                return term
    # 退路：按标点切段，取最长一段（≥3 字）且不出现在问题里
    pieces = [piece.strip() for piece in PHRASE_SPLIT.split(answer) if len(piece.strip()) >= 3]
    pieces.sort(key=len, reverse=True)
    for piece in pieces:
        if piece not in question:
            return piece
    return ""


def main() -> int:
    lines = []
    changed = 0
    for line in CASES.read_text(encoding="utf-8").splitlines():
        if not line.strip():
            continue
        case = json.loads(line)
        question = case["question"]
        answer = case["answer"]
        fact = pick_fact(answer, question)
        if not fact:
            raise SystemExit("用例 %s 挑不出只存在于答案里的事实词" % case["id"])
        if case.get("expect", {}).get("contains") != [fact]:
            changed += 1
        case["expect"] = {"contains": [fact]}
        lines.append(json.dumps(case, ensure_ascii=False))
    CASES.write_text("\n".join(lines) + "\n", encoding="utf-8")
    print("共 %d 条，改写 %d 条的事实词" % (len(lines), changed))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
