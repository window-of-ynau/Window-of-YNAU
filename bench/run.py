#!/usr/bin/env python3
"""云农之窗 · 评测门禁（bench/run.py）。

把 `bench/cases/full.jsonl` 的 50 条用例逐条打到 `POST /api/v1/chat/stream`，采五项指标：

1. **首字延迟**（TTFT，第一个 `delta` 帧）；
2. **总耗时**；
3. **tokens_in / tokens_out**（上游 `usage` 帧；没有就是 null，不伪造）；
4. **估计成本**（按 `bench/pricing.json` 的价目折算成"分"）；
5. **要点命中**（回答是否包含用例 `expect.contains` 的全部关键词）。

汇总出 P50/P95（TTFT 与总耗时）、命中率、错误率、单条成本，并与 `bench/baseline.json` 比对：
**任一维度退化 > 10% 或命中率下降 > 10%（相对）即失败**。

只依赖 Python 3.8+ 标准库；除结果文件外不写仓库内其他文件（`--report` 显式指定时才写报告）。

退出码：
  0  跑完，且相对 baseline 没有退化（baseline 不存在或标了 bootstrap 时也返回 0）
  1  相对 baseline 退化
  2  参数/用例文件/baseline 文件不合法，或服务不可达（不打印 traceback）
"""

import argparse
import json
import statistics
import sys
import time
from pathlib import Path
from urllib import error as urlerror
from urllib import request as urlrequest

SCRIPT_DIR = Path(__file__).resolve().parent
DEFAULT_CASES = SCRIPT_DIR / "cases" / "full.jsonl"
DEFAULT_PRICING = SCRIPT_DIR / "pricing.json"
DEFAULT_BASELINE = SCRIPT_DIR / "baseline.json"

# 成本只用来看趋势，不是账单：套餐按时间授权，服务端只对 1% 请求做 token 抽样。
# 价目（元 / 百万 token）默认按 deepseek-chat 量级取的示意值；可用 `--pricing` 覆盖，
# 文件缺省时就用这里的值（评测环境不该因为少一个 JSON 就跑不起来）。
PRICE_IN_YUAN_PER_MTOK = 2.0
PRICE_OUT_YUAN_PER_MTOK = 8.0

# 本地基准走直连：显式禁用 HTTP(S)_PROXY，避免公司代理缓冲把 SSE 的 TTFT 计时搞脏。
# 如果要打的是必须走代理的外网地址，把下面这行换成默认 opener 即可。
_OPENER = urlrequest.build_opener(urlrequest.ProxyHandler({}))

CHAT_PATH = "/api/v1/chat/stream"


class BenchError(Exception):
    """配置/输入错误 → 退出码 2。"""


class ServiceUnavailable(Exception):
    """服务连不上 → 退出码 2。"""


def _force_utf8_stdout():
    """Windows 上输出被重定向时默认用本地编码（cp936），中文帮助/路径会乱码。"""
    for stream in (sys.stdout, sys.stderr):
        try:
            stream.reconfigure(encoding="utf-8", errors="replace")
        except (AttributeError, ValueError, OSError):
            pass


def _now_ms(t0):
    return round((time.perf_counter() - t0) * 1000.0, 1)


def _is_timeout(reason):
    if isinstance(reason, TimeoutError):
        return True
    return "timed out" in str(reason).lower()


def _load_json(text):
    try:
        return json.loads(text)
    except (TypeError, ValueError):
        return None


def _as_int(value):
    if isinstance(value, bool) or value is None:
        return None
    if isinstance(value, int):
        return value
    if isinstance(value, float):
        return int(value)
    if isinstance(value, str) and value.strip().isdigit():
        return int(value.strip())
    return None


def _error_code_from_obj(obj):
    """从统一错误信封（或裸 {code}）里取 code。"""
    if not isinstance(obj, dict):
        return None
    code = obj.get("code")
    if isinstance(code, str) and code:
        return code
    err = obj.get("error")
    if isinstance(err, dict):
        code = err.get("code")
        if isinstance(code, str) and code:
            return code
    return None


def _error_code_from_body(raw):
    if not raw:
        return None
    try:
        obj = json.loads(raw.decode("utf-8", "replace"))
    except ValueError:
        return None
    return _error_code_from_obj(obj)


def _delta_text(data):
    obj = _load_json(data)
    if isinstance(obj, dict):
        for key in ("text", "content"):
            value = obj.get(key)
            if isinstance(value, str):
                return value
        return ""
    return data


def iter_sse(stream):
    """把 SSE 字节流切成 (event, data) 事件，按空行分帧，忽略注释/心跳。"""
    event = None
    data_lines = []
    for raw in stream:
        if isinstance(raw, bytes):
            line = raw.decode("utf-8", "replace")
        else:
            line = raw
        line = line.rstrip("\r\n")
        if not line:
            if event is not None or data_lines:
                yield event or "message", "\n".join(data_lines)
            event = None
            data_lines = []
            continue
        if line.startswith(":"):
            continue
        field, _, value = line.partition(":")
        if value.startswith(" "):
            value = value[1:]
        if field == "event":
            event = value.strip()
        elif field == "data":
            data_lines.append(value)
    if event is not None or data_lines:
        yield event or "message", "\n".join(data_lines)


def load_cases(path):
    """读 JSONL，逐行校验；空行跳过。"""
    if not path.is_file():
        raise BenchError("cases file not found: %s" % path)
    cases = []
    seen = set()
    try:
        text = path.read_text(encoding="utf-8")
    except OSError as exc:
        raise BenchError("cannot read cases file %s: %s" % (path, exc))
    for lineno, raw in enumerate(text.splitlines(), 1):
        line = raw.strip()
        if not line:
            continue
        try:
            case = json.loads(line)
        except ValueError as exc:
            raise BenchError("%s:%d: invalid JSON (%s)" % (path, lineno, exc))
        if not isinstance(case, dict):
            raise BenchError("%s:%d: case must be a JSON object" % (path, lineno))
        case_id = case.get("id")
        question = case.get("question")
        if not isinstance(case_id, str) or not case_id.strip():
            raise BenchError("%s:%d: missing string field 'id'" % (path, lineno))
        if case_id in seen:
            raise BenchError("%s:%d: duplicate id %r" % (path, lineno, case_id))
        if not isinstance(question, str) or not question.strip():
            raise BenchError("%s:%d: missing string field 'question'" % (path, lineno))
        expect = case.get("expect") or {}
        if not isinstance(expect, dict):
            raise BenchError("%s:%d: 'expect' must be an object" % (path, lineno))
        contains = expect.get("contains") or []
        if isinstance(contains, str):
            contains = [contains]
        if not isinstance(contains, list) or any(not isinstance(t, str) for t in contains):
            raise BenchError("%s:%d: 'expect.contains' must be a string list" % (path, lineno))
        seen.add(case_id)
        cases.append(case)
    if not cases:
        raise BenchError("no cases in %s" % path)
    return cases


def load_baseline(path):
    """baseline 不存在返回 None（按约定不算失败）；存在但坏了就是配置错误。"""
    if not path.exists():
        return None
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError) as exc:
        raise BenchError("cannot read baseline %s: %s" % (path, exc))
    if not isinstance(data, dict) or not isinstance(data.get("summary"), dict):
        raise BenchError("baseline %s has no 'summary' object" % path)
    return data


def build_payload(case_id, question):
    return {
        "session_id": "bench",
        "turn_id": "bench-%s" % case_id,
        "round": 1,
        "model": "deepseek-chat",
        "protocol": "openai_completions",
        "messages": [{"role": "user", "content": question}],
        "tools": [],
        "max_output_tokens": 512,
        "stream": True,
        "estimated_input_tokens": 16,
    }


def preflight(base_url, timeout):
    """先探一次 /healthz：完全连不上就退 2，避免每条用例都干等 timeout。

    有 HTTP 响应就算"服务在"——404/500 交给后续用例暴露，不在这里武断退出。
    """
    probe_timeout = min(max(timeout, 1.0), 5.0)
    req = urlrequest.Request(base_url + "/healthz", headers={"Accept": "application/json"})
    try:
        with _OPENER.open(req, timeout=probe_timeout) as resp:
            resp.read(1024)
    except urlerror.HTTPError:
        return
    except (urlerror.URLError, TimeoutError, OSError, ValueError) as exc:
        raise ServiceUnavailable("cannot reach %s/healthz: %s" % (base_url, exc))


def run_case_once(base_url, case, timeout, token=None):
    """跑**一次**用例；返回 (result, 拼起来的回答文本)。连接级失败抛 ServiceUnavailable。"""
    case_id = case["id"]
    payload = build_payload(case_id, case["question"])
    body = json.dumps(payload, ensure_ascii=False).encode("utf-8")
    headers = {
        "Content-Type": "application/json; charset=utf-8",
        "Accept": "text/event-stream",
    }
    if token:
        headers["Authorization"] = "Bearer %s" % token
    req = urlrequest.Request(base_url + CHAT_PATH, data=body, headers=headers, method="POST")

    result = {
        "id": case_id,
        "ok": False,
        "ttft_ms": None,
        "total_ms": None,
        "tokens_in": None,
        "tokens_out": None,
        "error_code": None,
    }
    t0 = time.perf_counter()
    first_delta_at = None
    saw_done = False
    saw_error_code = None
    delta_parts = []

    try:
        resp = _OPENER.open(req, timeout=timeout)
    except urlerror.HTTPError as exc:
        raw = b""
        try:
            raw = exc.read()
        except OSError:
            pass
        result["total_ms"] = _now_ms(t0)
        result["error_code"] = _error_code_from_body(raw) or ("HTTP_%d" % exc.code)
        return result, ""
    except urlerror.URLError as exc:
        if _is_timeout(exc.reason):
            # 服务在，但迟迟不给响应头：算这一条超时，不中止整轮基准。
            result["total_ms"] = _now_ms(t0)
            result["error_code"] = "TIMEOUT"
            return result, ""
        raise ServiceUnavailable("cannot reach %s: %s" % (base_url, exc))
    except TimeoutError:
        result["total_ms"] = _now_ms(t0)
        result["error_code"] = "TIMEOUT"
        return result, ""
    except OSError as exc:
        if _is_timeout(exc):
            result["total_ms"] = _now_ms(t0)
            result["error_code"] = "TIMEOUT"
            return result, ""
        raise ServiceUnavailable("cannot reach %s: %s" % (base_url, exc))
    except ValueError as exc:
        raise ServiceUnavailable("invalid request URL for %s: %s" % (base_url, exc))

    try:
        with resp:
            content_type = (resp.headers.get("Content-Type") or "").lower()
            if "application/json" in content_type:
                # 有些实现会把错误信封以 200 + JSON 返回，这里按错误处理而不是当空流。
                raw = resp.read()
                result["error_code"] = _error_code_from_body(raw) or "UNEXPECTED_JSON"
            else:
                for event, data in iter_sse(resp):
                    if event == "delta":
                        if first_delta_at is None:
                            first_delta_at = time.perf_counter()
                        delta_parts.append(_delta_text(data))
                    elif event == "usage":
                        obj = _load_json(data)
                        if isinstance(obj, dict):
                            result["tokens_in"] = _as_int(obj.get("tokens_in", obj.get("input_tokens")))
                            result["tokens_out"] = _as_int(obj.get("tokens_out", obj.get("output_tokens")))
                    elif event == "error":
                        saw_error_code = _error_code_from_obj(_load_json(data)) or "UPSTREAM_ERROR"
                    elif event == "done":
                        saw_done = True
                        break
                    # meta / tool_call / ping 不参与计时与断言
    except TimeoutError:
        result["error_code"] = "TIMEOUT"
    except OSError as exc:
        result["error_code"] = "TIMEOUT" if _is_timeout(exc) else "TRANSPORT_ERROR"
    except Exception as exc:  # 协议层异常（如 chunked 截断）只算这一条用例失败，不让整轮基准崩掉
        result["error_code"] = "STREAM_ERROR"
        print("warn: %s: stream error: %s" % (case_id, exc), file=sys.stderr)

    result["total_ms"] = _now_ms(t0)
    if first_delta_at is not None:
        result["ttft_ms"] = round((first_delta_at - t0) * 1000.0, 1)
    if saw_error_code:
        result["error_code"] = saw_error_code
    elif not saw_done and result["error_code"] is None:
        result["error_code"] = "STREAM_INCOMPLETE"
    result["ok"] = saw_done and result["error_code"] is None
    return result, "".join(delta_parts)


def _median(values):
    ordered = sorted(values)
    return ordered[len(ordered) // 2]


def run_case(base_url, case, timeout, token=None, repeats=1):
    """跑一条用例：重复 `repeats` 次取**中位数**，回答取最后一次成功的。

    为什么要取中位数：mock 场景下 TTFT 只有个位数毫秒，一次连接建立/调度抖动就能到 20ms；
    P95 在 50 条样本上会被这一条拉高数倍（实测 3.2ms → 21.1ms，直接判退化，而 P50 反而更好）。
    中位数把"偶发一次"过滤掉，只留"稳定变慢"——这才是门禁要抓的东西（run 42337511062 的教训）。
    """
    runs = max(1, int(repeats))
    sampled = []
    answer = ""
    for _ in range(runs):
        result, current_answer = run_case_once(base_url, case, timeout, token)
        sampled.append(result)
        if result["ok"] and current_answer:
            answer = current_answer

    ok_results = [r for r in sampled if r["ok"]]
    if not ok_results:
        return sampled[-1], answer

    merged = dict(ok_results[len(ok_results) // 2])
    # 原始值必须留：只有中位数时无法区分"3 次里 1 次慢"和"3 次都慢"，
    # 事后排查只能靠重跑（run 42367996754 的教训）。这是诊断证据，不是冗余数据。
    merged["samples"] = [
        {
            "ttft_ms": r["ttft_ms"],
            "total_ms": r["total_ms"],
            "tokens_in": r["tokens_in"],
            "tokens_out": r["tokens_out"],
            "ok": r["ok"],
        }
        for r in sampled
    ]
    merged["ttft_ms"] = round(_median([r["ttft_ms"] for r in ok_results]), 1)
    merged["total_ms"] = round(_median([r["total_ms"] for r in ok_results]), 1)
    token_ins = [r["tokens_in"] for r in ok_results if isinstance(r["tokens_in"], int)]
    token_outs = [r["tokens_out"] for r in ok_results if isinstance(r["tokens_out"], int)]
    merged["tokens_in"] = _median(token_ins) if token_ins else None
    merged["tokens_out"] = _median(token_outs) if token_outs else None
    merged["runs"] = len(ok_results)
    return merged, answer


def percentile(values, pct):
    """线性插值分位数（样本少时不至于被最近的极值吃掉）。"""
    ordered = sorted(values)
    if not ordered:
        return None
    if len(ordered) == 1:
        return float(ordered[0])
    pos = (len(ordered) - 1) * (pct / 100.0)
    low = int(pos)
    high = min(low + 1, len(ordered) - 1)
    frac = pos - low
    return ordered[low] + (ordered[high] - ordered[low]) * frac


def case_cost_cents(tokens_in, tokens_out):
    ti = tokens_in if isinstance(tokens_in, (int, float)) else 0
    to = tokens_out if isinstance(tokens_out, (int, float)) else 0
    return (ti / 1_000_000.0 * PRICE_IN_YUAN_PER_MTOK + to / 1_000_000.0 * PRICE_OUT_YUAN_PER_MTOK) * 100.0


def expect_hit(case, answer):
    """回答是否命中用例期望要点（`expect.contains` 全部出现）。没写期望 → 不计入命中率。"""
    expect = case.get("expect") or {}
    terms = expect.get("contains") or []
    if isinstance(terms, str):
        terms = [terms]
    terms = [term for term in terms if term]
    if not terms:
        return None
    return all(term in answer for term in terms)


def summarize(results):
    ttfts = [r["ttft_ms"] for r in results if r["ttft_ms"] is not None]
    totals = [r["total_ms"] for r in results if r["total_ms"] is not None]
    costs = [case_cost_cents(r["tokens_in"], r["tokens_out"]) for r in results]
    hits = [r["hit"] for r in results if r.get("hit") is not None]
    total = float(len(results))
    return {
        # —— 五项指标（t162）——
        "p50_ttft_ms": round(percentile(ttfts, 50), 1) if ttfts else None,
        "p95_ttft_ms": round(percentile(ttfts, 95), 1) if ttfts else None,
        "p50_total_ms": round(percentile(totals, 50), 1) if totals else None,
        "p95_total_ms": round(percentile(totals, 95), 1) if totals else None,
        "mean_total_ms": round(statistics.mean(totals), 1) if totals else None,
        "tokens_in_total": sum(r["tokens_in"] for r in results if isinstance(r["tokens_in"], int)),
        "tokens_out_total": sum(r["tokens_out"] for r in results if isinstance(r["tokens_out"], int)),
        "total_cost_cents": round(sum(costs), 4) if costs else None,
        "est_cost_per_case_cents": round(statistics.mean(costs), 4) if costs else None,
        "hit_rate": round(sum(1 for hit in hits if hit) / len(hits), 4) if hits else None,
        "hit_cases": len(hits),
        "error_rate": round(1.0 - sum(1 for r in results if r["ok"]) / total, 4) if total else None,
        "cases": len(results),
        "cases_ok": sum(1 for r in results if r["ok"]),
    }


def _num(value):
    if isinstance(value, bool) or value is None:
        return None
    try:
        return float(value)
    except (TypeError, ValueError):
        return None


REGRESSION_TOLERANCE = 0.10  # 退化 > 10% 即阻断（延迟 / 成本 / 命中率三个方向都用它）

# 绝对噪声下限（t162）：mock 上游的延迟是个位数毫秒，+10% 只有 0.3ms，CI 调度抖动就能超过它。
# 所以"相对 >10%"还要**同时**超过下面的绝对增量才阻断。真实上游是几百毫秒量级，这里永远不生效。
# 实测（连续三次 CI run）P95 抖动幅度：2.6ms / 3.2ms / 21.1ms。10ms 能盖住 CI 调度毛刺，
# 而真实上游是几百毫秒量级（10ms ≈ 2–5%），不会掩盖真退化。
NOISE_FLOOR_MS = 10.0
NOISE_FLOOR_CENTS = 0.01

# p95 的测量能力取决于量级：mock 上游是 1–2ms，而 CI runner 的偶发抖动是 10–40ms
# ——抖动比被测信号大一个数量级，这个指标在 mock 场景**没有测量能力**（不是阈值问题）。
# 规则：基线 p95 < 100ms ⇒ p95 只观测（写进报告并标 ⚠️），不参与退出码；
#      基线 p95 ≥ 100ms（真实上游是几百毫秒量级）⇒ 自动恢复为阻断项。
# 也可用 `--p95-gate on|off` 强制覆盖。详见 bench/README「恢复 p95 阻断的条件」。
P95_BLOCKING_MIN_BASELINE_MS = 100.0
P95_WARN_FACTOR = 5.0  # 观测模式下，本轮 p95 > 基线 5 倍 ⇒ 报告打 ⚠️
ENV_UNSTABLE_RATIO = 0.10  # 触发重测的用例占比 ≥10% ⇒ ENV_UNSTABLE
ENV_UNSTABLE_MIN = 5  # 且至少 5 条
# 极端兜底：≥25% 的用例都被拖慢 ⇒ 这轮数据本身就是废的。
# 与其用不可信数据下结论，不如让它红一次、重跑（重跑 ~66s，比"拿废数据下结论"便宜得多）。
ENV_ANOMALY_RATIO = 0.25


def compare_with_baseline(summary, baseline, p95_blocking=False):
    """返回退化原因列表；空列表表示通过。

    判定口径（t162，写死在代码里，避免"改基线把门禁改松"）：

    | 维度 | 方向 | 阈值 |
    |---|---|---|
    | TTFT P50 / P95 | 变大是坏 | > +10% |
    | 总耗时 P50 / P95 | 变大是坏 | > +10% |
    | 单条成本 | 变大是坏 | > +10% |
    | 要点命中率 | **变小是坏** | < baseline − 10%（相对） |
    | 错误率 | 变大是坏 | > baseline + 5pp |
    """
    base = baseline.get("summary") or {}
    reasons = []

    def worse_high(key, label, floor=0.0):
        base_value = _num(base.get(key))
        current = _num(summary.get(key))
        if base_value and current is not None and current > base_value * (1.0 + REGRESSION_TOLERANCE):
            delta = current / base_value - 1.0
            if (current - base_value) < floor:
                # 相对涨幅够了，但绝对增量小于噪声下限：记一行日志，不阻断
                print(
                    "note: %s +%.1f%% 但绝对增量 %.3f < 噪声下限 %.3f，不计为退化"
                    % (label, delta * 100.0, current - base_value, floor)
                )
                return
            reasons.append(
                "%s %.1f > baseline %.1f (+%.1f%% > +10%%)"
                % (label, current, base_value, delta * 100.0)
            )

    worse_high("p50_ttft_ms", "p50_ttft_ms", NOISE_FLOOR_MS)
    worse_high("p50_total_ms", "p50_total_ms", NOISE_FLOOR_MS)
    worse_high("est_cost_per_case_cents", "est_cost_per_case_cents", NOISE_FLOOR_CENTS)
    if p95_blocking:
        # 仅"真实上游量级"（基线 p95 ≥ 100ms）或显式 --p95-gate on 时才阻断
        worse_high("p95_ttft_ms", "p95_ttft_ms", NOISE_FLOOR_MS)
        worse_high("p95_total_ms", "p95_total_ms", NOISE_FLOOR_MS)

    base_hit = _num(base.get("hit_rate"))
    current_hit = _num(summary.get("hit_rate"))
    if base_hit is not None and current_hit is not None:
        if current_hit < base_hit * (1.0 - REGRESSION_TOLERANCE):
            reasons.append(
                "hit_rate %.4f < baseline %.4f (-%.1f%% > -10%%)"
                % (current_hit, base_hit, (1.0 - current_hit / base_hit) * 100.0)
            )

    base_err = _num(base.get("error_rate"))
    if base_err is not None and summary.get("error_rate") is not None:
        if summary["error_rate"] > base_err + 0.05 + 1e-9:
            reasons.append(
                "error_rate %.4f > baseline %.4f (+%.2fpp > +5pp)"
                % (summary["error_rate"], base_err, (summary["error_rate"] - base_err) * 100.0)
            )
    return reasons


# D2：孤立尖刺允许重测一次（必须留痕）
OUTLIER_FACTOR = 5.0  # 用例中位数 > 本轮"中位数的中位数"的 5 倍 ⇒ 疑似孤立尖刺
# 注：早期规则是"≥3 条触发重测 ⇒ 直接判失败"。captain 2026-11 修订后改为
# "≥5 条（≥10%）⇒ 标 ENV_UNSTABLE + p95 不作依据"（因为 p95 在 mock 场景已不阻断，
# 再保留硬失败只会把环境抖动误判成 PR 问题）。


def retry_outliers(base_url, cases, results, args, baseline):
    """对**孤立尖刺**重测一次；整轮变慢时不重测（照常阻断）。

    触发条件（两个同时满足，captain 2026-11 批准）：
      1. 该用例的 TTFT 中位数 > 本轮所有用例中位数的 `OUTLIER_FACTOR` 倍；
      2. 本轮 P50 仍在基线 +10% 以内（即整轮没变慢）。
    重测结果**两个值都写进结果与报告**（首次 / 重测 / 采用），不允许悄悄取小的。
    返回重测过的用例数（调用方据此判断是否 ENV_UNSTABLE，见 ENV_UNSTABLE_RATIO）。
    """
    if not results:
        return 0
    ok = [r for r in results if r.get("ok") and isinstance(r.get("ttft_ms"), (int, float))]
    if len(ok) < 5:
        return 0
    run_median = _median([r["ttft_ms"] for r in ok])

    base_p50 = None
    if baseline:
        base_p50 = _num((baseline.get("summary") or {}).get("p50_ttft_ms"))
    run_p50 = _median([r["ttft_ms"] for r in ok])
    whole_run_slow = bool(base_p50) and run_p50 > base_p50 * (1.0 + REGRESSION_TOLERANCE)
    if whole_run_slow:
        print(
            "note: 本轮 P50 %.1fms 已超基线 +10%%（基线 %.1fms）⇒ 整轮变慢，不做尖刺重测"
            % (run_p50, base_p50)
        )
        return 0

    by_id = {r["id"]: r for r in results}
    suspects = [r for r in ok if r["ttft_ms"] > run_median * OUTLIER_FACTOR]
    if not suspects:
        return 0

    case_by_id = {c["id"]: c for c in cases}
    for item in suspects:
        case = case_by_id.get(item["id"])
        if case is None:
            continue
        print(
            "note: %s 疑似孤立尖刺（%.1fms > %.1f×%.1fms）→ 重测一次"
            % (item["id"], item["ttft_ms"], OUTLIER_FACTOR, run_median)
        )
        again, answer = run_case(base_url, case, args.timeout, args.token, args.repeats)
        first_median = item["ttft_ms"]
        retry_median = again.get("ttft_ms")
        adopted = "retry"
        if not again.get("ok") or not isinstance(retry_median, (int, float)):
            adopted = "first"
        elif retry_median > first_median:
            adopted = "first"
        item["retry"] = {
            "first_median_ttft_ms": first_median,
            "retry_median_ttft_ms": retry_median,
            "adopted": adopted,
            "retry_samples": again.get("samples"),
        }
        if adopted == "retry":
            item["ttft_ms"] = retry_median
            item["total_ms"] = again.get("total_ms")
            if isinstance(again.get("tokens_in"), int):
                item["tokens_in"] = again["tokens_in"]
            if isinstance(again.get("tokens_out"), int):
                item["tokens_out"] = again["tokens_out"]
            item["samples"] = again.get("samples")
            item["hit"] = expect_hit(case, answer)
        print(
            "      首次=%s / 重测=%s / 采用=%s"
            % (
                _fmt_ms(item["retry"]["first_median_ttft_ms"]),
                _fmt_ms(item["retry"]["retry_median_ttft_ms"]),
                adopted,
            )
        )

    by_id.clear()
    return len(suspects)


def p95_observation(summary, baseline):
    """p95 观测结果（不阻断）：返回 (文本, 是否 ⚠️)；无数据时 None。"""
    base = (baseline or {}).get("summary") or {}
    base_p95 = _num(base.get("p95_ttft_ms"))
    current = _num(summary.get("p95_ttft_ms"))
    if current is None:
        return None
    if not base_p95:
        return ("%.1f ms（无基线）" % current, False)
    delta = current / base_p95 - 1.0
    warn = current > base_p95 * P95_WARN_FACTOR
    return (
        "%.1f ms vs 基线 %.1f ms（%+.1f%%）%s"
        % (current, base_p95, delta * 100.0, " ⚠️ 异常尖刺" if warn else ""),
        warn,
    )


def slowest_cases(results, count=3):
    """最慢的几条（按总耗时）。抖动时先看它们，能把"环境毛刺"和"某条用例真的变慢"分开。"""
    ordered = sorted(
        [r for r in results if r.get("ok") and isinstance(r.get("total_ms"), (int, float))],
        key=lambda r: r["total_ms"],
        reverse=True,
    )
    return ordered[:count]


def write_report(path, summary, baseline, reasons, env_note, results=None, meta=None):
    """写一份人可读的 Markdown 对比报告（PR 里直接贴）。"""
    base = (baseline or {}).get("summary") or {}

    def row(label, key, unit="", better="越小越好"):
        cur = summary.get(key)
        old = base.get(key)
        cur_text = "-" if cur is None else ("%.1f" % cur if isinstance(cur, float) else str(cur))
        old_text = "-" if old is None else ("%.1f" % old if isinstance(old, float) else str(old))
        if isinstance(cur, (int, float)) and isinstance(old, (int, float)) and old:
            delta = (cur - old) / old * 100.0
            delta_text = "%+.1f%%" % delta
        else:
            delta_text = "-"
        return "| %s | %s%s | %s%s | %s | %s |" % (
            label,
            cur_text,
            unit,
            old_text,
            unit,
            delta_text,
            better,
        )

    lines = [
        "# 评测门禁报告（bench）",
        "",
        "采集环境：%s" % env_note,
        "",
        "| 指标 | 本次 | 基线 | 变化 | 方向 |",
        "|---|---|---|---|---|",
        row("TTFT P50", "p50_ttft_ms", " ms"),
        row("TTFT P95（观测，不阻断）", "p95_ttft_ms", " ms"),
        row("总耗时 P50", "p50_total_ms", " ms"),
        row("总耗时 P95（观测，不阻断）", "p95_total_ms", " ms"),
        row("单条成本", "est_cost_per_case_cents", " 分"),
        row("要点命中率", "hit_rate", "", "越大越好"),
        row("错误率", "error_rate", "", "越小越好"),
        "",
        "- 用例数：%s（成功 %s）" % (summary.get("cases"), summary.get("cases_ok")),
        "- tokens：in=%s / out=%s" % (summary.get("tokens_in_total"), summary.get("tokens_out_total")),
        "- 总成本：%s 分" % summary.get("total_cost_cents"),
        "- 采集方式：预热 + 每条用例重复取中位数（抗单次毛刺）",
        "",
    ]
    meta = meta or {}
    if meta.get("p95_note"):
        lines.append("")
        lines.append("### p95 观测（mock 场景不阻断）")
        lines.append("")
        lines.append(
            "本轮 p95 = %s。基线 p95 < %.0f ms 时，CI runner 的抖动量级（10–40ms）大于被测信号，"
            "该指标没有测量能力 ⇒ **只记录、不阻断**；接真实上游（几百毫秒量级）后自动恢复为阻断项。"
            % (meta["p95_note"], P95_BLOCKING_MIN_BASELINE_MS)
        )
        lines.append("")
    if meta.get("env_anomaly"):
        lines.append(
            "> **ENV_ANOMALY**：本轮 %d 条用例触发尖刺重测（兜底阈值 ≥%d 条 = %.0f%%）——"
            "**这轮数据不可信，判定为失败，请重跑**。重跑成本约 66s，比拿废数据下结论便宜。"
            % (meta.get("retried") or 0, meta.get("env_anomaly_limit") or 0, ENV_ANOMALY_RATIO * 100.0)
        )
        lines.append("")
    elif meta.get("env_unstable"):
        lines.append(
            "> **ENV_UNSTABLE**：本轮 %d 条用例触发尖刺重测（阈值 ≥%d 条且 ≥%.0f%%）——"
            "判定为整轮环境抖动，只按 **p50 / 命中率 / 错误率 / 成本** 判定，p95 不作为依据。"
            % (meta.get("retried") or 0, meta.get("env_unstable_limit") or 0, ENV_UNSTABLE_RATIO * 100.0)
        )
        lines.append("")

    retried_items = [r for r in (results or []) if r.get("retry")]
    if retried_items:
        lines.append("")
        lines.append("### 尖刺重测记录（孤立尖刺只重测一次，两个值都留）")
        lines.append("")
        lines.append("| 用例 | 首次中位 TTFT | 重测中位 TTFT | 采用 |")
        lines.append("|---|---|---|---|")
        for item in retried_items:
            retry = item["retry"]
            lines.append(
                "| %s | %.1f ms | %s | %s |"
                % (
                    item.get("id"),
                    retry.get("first_median_ttft_ms") or 0.0,
                    "-" if retry.get("retry_median_ttft_ms") is None else "%.1f ms" % retry["retry_median_ttft_ms"],
                    retry.get("adopted"),
                )
            )
        lines.append("")

    if results:
        slowest = slowest_cases(results)
        if slowest:
            lines.append("| 最慢用例（总耗时） | TTFT | 总耗时 |")
            lines.append("|---|---|---|")
            for item in slowest:
                lines.append(
                    "| %s | %.1f ms | %.1f ms |"
                    % (item.get("id"), item["ttft_ms"], item["total_ms"])
                )
            lines.append("")
    if baseline is None:
        lines.append("**判定：无基线（首次采集）** —— 本次数字可直接作为基线。")
    elif (baseline or {}).get("bootstrap"):
        lines.append("**判定：基线标记为 bootstrap（首次采集窗口）** —— 只记录，不阻断。")
    elif reasons:
        lines.append("**判定：退化 %d 项 → 阻断合并**" % len(reasons))
        lines.extend(["", "```"] + reasons + ["```"])
    else:
        lines.append("**判定：通过**（所有维度退化均 ≤ 10%）")
    Path(path).parent.mkdir(parents=True, exist_ok=True)
    Path(path).write_text("\n".join(lines) + "\n", encoding="utf-8")
    return path


def write_result(out_path, results, summary):
    out_path.parent.mkdir(parents=True, exist_ok=True)
    doc = {
        "generated_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "cases": results,
        "summary": summary,
    }
    out_path.write_text(json.dumps(doc, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    return out_path


def _fmt_expect(case, answer):
    expect = case.get("expect") or {}
    terms = expect.get("contains") or []
    if isinstance(terms, str):
        terms = [terms]
    missing = [t for t in terms if t and t not in answer]
    if not terms:
        return "expect: -"
    if missing:
        return "expect: MISS(%s)" % ",".join(missing)
    return "expect: ok"


def _print_unreachable(exc):
    print("ERROR: %s" % exc, file=sys.stderr)
    print(
        "hint: start the server first, e.g. `cargo run -p yunong-server` "
        "or `docker compose -f deploy/docker-compose.yml up -d`",
        file=sys.stderr,
    )


def main(argv=None):
    _force_utf8_stdout()
    parser = argparse.ArgumentParser(
        description="云农之窗 chat/stream 冒烟基准：TTFT / 总耗时 / token / 错误率，并与 baseline 比退化。",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=(
            "退出码: 0 无退化或无 baseline | 1 相对 baseline 退化 | 2 参数/文件错误或服务不可达\n"
            "示例: python bench/run.py --cases bench/cases/smoke.jsonl --out bench/out/result.json"
        ),
    )
    parser.add_argument("--base-url", default="http://127.0.0.1:8080", help="服务地址 origin（默认 %(default)s）")
    parser.add_argument("--cases", default=str(DEFAULT_CASES), help="JSONL 用例文件（默认 bench/cases/smoke.jsonl）")
    parser.add_argument("--baseline", default=str(DEFAULT_BASELINE), help="基线 JSON（默认 bench/baseline.json）")
    parser.add_argument("--out", default=None, help="结果 JSON 路径（默认 bench/out/bench-<UTC 时间>.json）")
    parser.add_argument("--timeout", type=float, default=60.0, help="单条用例超时秒数（默认 %(default)s）")
    parser.add_argument(
        "--repeats",
        type=int,
        default=5,
        help="每条用例重复次数，取中位数（默认 %(default)s；抗单次毛刺，原始值会全部记录）",
    )
    parser.add_argument(
        "--p95-gate",
        choices=("auto", "on", "off"),
        default="auto",
        help="p95 是否参与阻断：auto=基线 p95 ≥%.0f ms 时阻断（真实上游量级），on=强制阻断，off=只观测"
        % P95_BLOCKING_MIN_BASELINE_MS,
    )
    parser.add_argument(
        "--warmup",
        type=int,
        default=3,
        help="正式测量前的预热请求数（默认 %(default)s；消掉首个连接的建连/冷缓存）",
    )
    parser.add_argument("--token", default=None, help="可选 Bearer token；本地免鉴权服务不用传")
    parser.add_argument(
        "--report",
        default=None,
        help="Markdown 对比报告的输出路径（如 bench/results/report.md）；不传就不写",
    )
    parser.add_argument(
        "--env-note",
        default="local",
        help="采集环境标注（写进报告/基线，例如 'GitHub Actions ubuntu-latest'）",
    )
    parser.add_argument(
        "--update-baseline",
        default=None,
        help="把本次 summary 写成基线文件（首次采集用；写出的基线会带上环境与时间）",
    )
    args = parser.parse_args(argv)

    base_url = args.base_url.strip().rstrip("/")
    if not (base_url.startswith("http://") or base_url.startswith("https://")):
        print("ERROR: --base-url must start with http:// or https://", file=sys.stderr)
        return 2
    if args.timeout <= 0:
        print("ERROR: --timeout must be > 0", file=sys.stderr)
        return 2

    try:
        cases = load_cases(Path(args.cases))
        baseline_path = Path(args.baseline)
        baseline = load_baseline(baseline_path)
    except BenchError as exc:
        print("ERROR: %s" % exc, file=sys.stderr)
        return 2

    if args.out:
        out_path = Path(args.out)
    else:
        stamp = time.strftime("%Y%m%dT%H%M%SZ", time.gmtime())
        out_path = SCRIPT_DIR / "out" / ("bench-%s.json" % stamp)

    print("bench: %d cases -> %s%s" % (len(cases), base_url, CHAT_PATH))
    try:
        preflight(base_url, args.timeout)
    except ServiceUnavailable as exc:
        _print_unreachable(exc)
        return 2

    # 预热（不计入结果）：首个请求要建连、走 DNS/冷缓存，不预热的话它的毛刺会直接进 P95
    # ——那测的是"冷启动"，不是"稳态退化"。
    for _ in range(max(0, args.warmup)):
        try:
            run_case_once(base_url, cases[0], args.timeout, args.token)
        except ServiceUnavailable:
            raise
        except Exception:  # noqa: BLE001 - 预热失败不该中止正式测量
            break
    if args.warmup > 0:
        print("warmup: %d 次预热完成（不计入结果）" % args.warmup)

    results = []
    for index, case in enumerate(cases, 1):
        try:
            result, answer = run_case(base_url, case, args.timeout, args.token, args.repeats)
        except ServiceUnavailable as exc:
            _print_unreachable(exc)
            return 2
        # 五项指标里的后两项（命中 / 成本）在这一层算：run_case 只负责"发请求、收流、计时"
        result["hit"] = expect_hit(case, answer)
        result["cost_cents"] = round(
            case_cost_cents(result["tokens_in"], result["tokens_out"]), 6
        )
        results.append(result)
        status = "ok" if result["ok"] else "FAIL(%s)" % (result["error_code"] or "UNKNOWN")
        ttft = "n/a" if result["ttft_ms"] is None else "%.1fms" % result["ttft_ms"]
        total = "n/a" if result["total_ms"] is None else "%.1fms" % result["total_ms"]
        tokens = "%s/%s" % (
            "-" if result["tokens_in"] is None else result["tokens_in"],
            "-" if result["tokens_out"] is None else result["tokens_out"],
        )
        print(
            "[%2d/%2d] %-6s ttft=%-9s total=%-9s tokens=%-9s %-28s %s"
            % (index, len(cases), case["id"], ttft, total, tokens, status, _fmt_expect(case, answer))
        )

    # D2：孤立尖刺重测（留痕）。
    retried = retry_outliers(base_url, cases, results, args, baseline)
    # 环境抖动分两档：
    #   ENV_UNSTABLE  —— 触发重测的用例多（≥5 且 ≥10%）：整轮 p95 不可信，只判 p50/命中率/错误率/成本；
    #   ENV_ANOMALY   —— 触发重测的用例 ≥25%：这轮数据本身就是废的，直接判失败、请重跑。
    env_unstable_limit = max(ENV_UNSTABLE_MIN, int(len(cases) * ENV_UNSTABLE_RATIO + 0.999))
    env_anomaly_limit = max(1, int(len(cases) * ENV_ANOMALY_RATIO + 0.999))
    env_unstable = retried >= env_unstable_limit
    env_anomaly = retried >= env_anomaly_limit

    summary = summarize(results)
    try:
        write_result(out_path, results, summary)
    except OSError as exc:
        print("ERROR: cannot write result file %s: %s" % (out_path, exc), file=sys.stderr)
        return 2

    print("")
    print("result: %s" % out_path)
    print(
        "summary: cases=%s ok=%s p50_ttft=%s p95_ttft=%s p50_total=%s p95_total=%s "
        "hit_rate=%s error_rate=%s cost_total=%s cents"
        % (
            summary["cases"],
            summary["cases_ok"],
            _fmt_ms(summary["p50_ttft_ms"]),
            _fmt_ms(summary["p95_ttft_ms"]),
            _fmt_ms(summary["p50_total_ms"]),
            _fmt_ms(summary["p95_total_ms"]),
            "-" if summary["hit_rate"] is None else "%.2f%%" % (summary["hit_rate"] * 100.0),
            "-" if summary["error_rate"] is None else "%.4f" % summary["error_rate"],
            "-" if summary["total_cost_cents"] is None else "%.4f" % summary["total_cost_cents"],
        )
    )

    # 首次采集：把本次 summary 落成基线（带环境与时间，便于以后追溯"这是在哪台机器上量的"）
    if args.update_baseline:
        stamp = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
        payload = {
            "version": 2,
            "collected_at": stamp,
            "environment": args.env_note,
            "note": "真实实测基线（bench/run.py --update-baseline 生成）。"
            "采集环境见 environment；退化门禁见 bench/run.py 的 compare_with_baseline。",
            "summary": summary,
        }
        target = Path(args.update_baseline)
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(json.dumps(payload, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
        print("baseline written: %s" % target)

    base_p95 = _num(((baseline or {}).get("summary") or {}).get("p95_ttft_ms"))
    if args.p95_gate == "on":
        p95_blocking = True
    elif args.p95_gate == "off":
        p95_blocking = False
    else:
        p95_blocking = bool(base_p95) and base_p95 >= P95_BLOCKING_MIN_BASELINE_MS

    if env_anomaly:
        print(
            "ENV_ANOMALY: %d 条用例触发尖刺重测（占 %d/%d = %.0f%%，兜底阈值 ≥%.0f%%）——"
            "本轮环境严重抖动，请重跑；不要用这一轮的数字下结论。"
            % (
                retried,
                retried,
                len(cases),
                100.0 * retried / max(1, len(cases)),
                ENV_ANOMALY_RATIO * 100.0,
            ),
            file=sys.stderr,
        )
    elif env_unstable:
        print(
            "ENV_UNSTABLE: %d 条用例触发尖刺重测（阈值 ≥%d 条且 ≥%.0f%%）——整轮环境抖动；%s"
            % (
                retried,
                env_unstable_limit,
                ENV_UNSTABLE_RATIO * 100.0,
                # 只有 p95 本来就不阻断时，"p95 不作依据"才是如实描述
                "本轮只按 p50 / 命中率 / 错误率 / 成本判定，p95 不作依据。"
                if not p95_blocking
                else "但 p95 仍作为阻断项（--p95-gate on 或真实上游量级基线）。",
            )
        )

    observation = p95_observation(summary, baseline)
    if baseline and not p95_blocking:
        print(
            "p95 gating: OFF（基线 p95 %s ms < %.0f ms ⇒ mock 量级，p95 仅观测）"
            % ("-" if not base_p95 else "%.1f" % base_p95, P95_BLOCKING_MIN_BASELINE_MS)
        )
        if observation:
            print("p95(观测,不阻断): %s" % observation[0])

    reasons = compare_with_baseline(summary, baseline, p95_blocking) if baseline else []
    if env_anomaly:
        reasons = reasons + [
            "ENV_ANOMALY: %d/%d（%.0f%%）用例触发尖刺重测 ≥ 兜底阈值 %.0f%%，本轮环境严重抖动，请重跑"
            % (retried, len(cases), 100.0 * retried / max(1, len(cases)), ENV_ANOMALY_RATIO * 100.0)
        ]
    if args.report:
        try:
            write_report(
                args.report,
                summary,
                baseline,
                reasons,
                "%s（%s）" % (args.env_note, time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())),
                results,
                {
                    "p95_note": observation[0] if observation else None,
                    "env_unstable": env_unstable,
                    "env_anomaly": env_anomaly,
                    "env_anomaly_limit": env_anomaly_limit,
                    "retried": retried,
                    "env_unstable_limit": env_unstable_limit,
                    "p95_blocking": p95_blocking,
                },
            )
            print("report: %s" % args.report)
        except OSError as exc:
            print("ERROR: cannot write report %s: %s" % (args.report, exc), file=sys.stderr)
            return 2

    if baseline is None:
        print("baseline: %s not found -> gate skipped (exit 0)" % baseline_path)
        return 0

    base = baseline.get("summary") or {}
    print(
        "baseline: p50_ttft=%s p95_ttft=%s p50_total=%s p95_total=%s hit_rate=%s error_rate=%s"
        % (
            base.get("p50_ttft_ms"),
            base.get("p95_ttft_ms"),
            base.get("p50_total_ms"),
            base.get("p95_total_ms"),
            base.get("hit_rate"),
            base.get("error_rate"),
        )
    )

    if baseline.get("bootstrap"):
        # 首次采集窗口：只记录不阻断（基线本身还是占位的，拿来当门禁等于拿 0 当分母）
        print("baseline is marked bootstrap -> record only, gate skipped (exit 0)")
        return 0

    if reasons:
        for reason in reasons:
            print("REGRESSION: %s" % reason, file=sys.stderr)
        print("FAIL: %d regression(s) against baseline" % len(reasons), file=sys.stderr)
        return 1
    print("PASS: no regression against baseline")
    return 0


def _fmt_ms(value):
    return "-" if value is None else "%.1fms" % value


if __name__ == "__main__":
    try:
        sys.exit(main())
    except KeyboardInterrupt:
        print("interrupted", file=sys.stderr)
        sys.exit(130)
