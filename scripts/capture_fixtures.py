#!/usr/bin/env python3
"""T1: fixture capture + format verification harness for meterly.

Modes:
  (default)            Generate fixtures/ (allowlist-projected samples from real
                       logs for structure + plan-specified fixed values).
  --verify-scrub       Recursively verify every fixtures/**/*.jsonl contains only
                       allowlisted keys. Non-zero exit on any violation.
  --survey             One-shot full-log verification scan for README items
                       (a)-(h). Heavy on Codex side (~6GB read) -- budget: run
                       once per verification cycle. Use --survey-limit N to test
                       on a subset first.

Privacy contract (plan: meterly-v1.md, B6): fixtures are produced by ALLOWLIST
PROJECTION -- only the keys the parsers need are emitted, everything else
(conversation text, cwd, tool i/o, thinking, ...) is structurally impossible to
leak because it is never copied. Real logs are opened read-only and never
modified. Real session ids / project slugs are replaced by synthetic ids.
"""

import argparse
import glob
import json
import os
import re
import sys

HOME = os.path.expanduser("~")
CLAUDE_ROOT = os.path.join(HOME, ".claude", "projects")
CODEX_SESSIONS = os.path.join(HOME, ".codex", "sessions")
CODEX_ARCHIVED = os.path.join(HOME, ".codex", "archived_sessions")

REPO_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
FIXTURES_DIR = os.path.join(REPO_ROOT, "fixtures")

UUID_RE = re.compile(
    r"([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})\.jsonl$"
)

# --------------------------------------------------------------------------
# Allowlist (plan "픽스처 스크럽 계약"). Dot paths; "**" = any subtree.
# --------------------------------------------------------------------------

CLAUDE_ALLOWED = [
    "type",
    "timestamp",
    "sessionId",  # synthetic only in fixtures
    "requestId",
    "message",
    "message.id",
    "message.model",
    "message.usage",
    "message.usage.**",
]

CODEX_ALLOWED = [
    "type",
    "timestamp",
    "payload",
    "payload.type",
    "payload.info",  # may be null
    "payload.info.total_token_usage",
    "payload.info.total_token_usage.**",
    "payload.info.last_token_usage",
    "payload.info.last_token_usage.**",
    "payload.rate_limits",
    "payload.rate_limits.**",
    "payload.model",  # T1(e): turn_context model attribution
]

# Intentionally-broken lines emitted into */malformed.jsonl. verify-scrub
# accepts an unparseable line only if it is one of these exact constants.
KNOWN_MALFORMED_LINES = {
    '{"type": "assistant", "timestamp": "2026-07-09T01:59:00.000Z", "message": {',
    'this is not json at all {{{',
    '{"timestamp": "2026-07-09T02:59:00.000Z", "type": "event_msg", "payload": {"type": "token_c',
}


def _match(path, allowed):
    for a in allowed:
        if a == path:
            return True
        if a.endswith(".**") and path.startswith(a[:-2]):
            return True
    return False


def _walk_paths(obj, prefix=""):
    """Yield every dict-key dot path in obj."""
    if isinstance(obj, dict):
        for k, v in obj.items():
            p = f"{prefix}.{k}" if prefix else k
            yield p
            yield from _walk_paths(v, p)
    elif isinstance(obj, list):
        for item in obj:
            yield from _walk_paths(item, prefix)


def project(obj, allowed, prefix=""):
    """Allowlist projection: keep only allowed key paths of obj."""
    if isinstance(obj, dict):
        out = {}
        for k, v in obj.items():
            p = f"{prefix}.{k}" if prefix else k
            if not _match(p, allowed):
                continue
            out[k] = project(v, allowed, p)
        return out
    if isinstance(obj, list):
        return [project(x, allowed, prefix) for x in obj]
    return obj


# --------------------------------------------------------------------------
# Real-log sampling (read-only; newest files first)
# --------------------------------------------------------------------------

def _claude_files():
    return sorted(
        glob.glob(os.path.join(CLAUDE_ROOT, "**", "*.jsonl"), recursive=True),
        key=os.path.getmtime,
        reverse=True,
    )


def _codex_files(include_archived=True):
    files = glob.glob(os.path.join(CODEX_SESSIONS, "**", "*.jsonl"), recursive=True)
    if include_archived:
        files += glob.glob(
            os.path.join(CODEX_ARCHIVED, "**", "*.jsonl"), recursive=True
        )
    return sorted(files, key=os.path.getmtime, reverse=True)


def _iter_records(path):
    with open(path, "r", errors="replace") as fh:
        for line in fh:
            line = line.strip()
            if not line:
                continue
            try:
                yield json.loads(line)
            except json.JSONDecodeError:
                continue


def sample_claude_assistant():
    for f in _claude_files():
        for rec in _iter_records(f):
            if rec.get("type") == "assistant" and isinstance(
                (rec.get("message") or {}).get("usage"), dict
            ):
                return project(rec, CLAUDE_ALLOWED)
    raise RuntimeError("no claude assistant record with usage found")


def sample_codex(kind):
    """kind: 'token_count' | 'turn_context' | 'info_null'"""
    for f in _codex_files():
        for rec in _iter_records(f):
            p = rec.get("payload")
            if not isinstance(p, dict):
                continue
            if kind == "turn_context" and rec.get("type") == "turn_context":
                if p.get("model"):
                    return project(rec, CODEX_ALLOWED)
            if p.get("type") == "token_count":
                if kind == "token_count" and isinstance(p.get("info"), dict):
                    return project(rec, CODEX_ALLOWED)
                if kind == "info_null" and p.get("info") is None:
                    return project(rec, CODEX_ALLOWED)
    raise RuntimeError(f"no codex sample found for kind={kind}")


# --------------------------------------------------------------------------
# Fixture builders (plan-specified fixed values on top of sampled structure)
# --------------------------------------------------------------------------

CLAUDE_SESSION_A = "aaaaaaaa-0000-4000-8000-00000000000a"
CLAUDE_SESSION_B = "bbbbbbbb-0000-4000-8000-00000000000b"
CODEX_DUP_UUID = "0195aaaa-1111-7000-8000-000000000001"
CODEX_MOVED_UUID = "0195bbbb-2222-7000-8000-000000000002"

FIXED_RATE_LIMITS = {
    "primary": {
        "used_percent": 25.0,
        "window_minutes": 300,
        "resets_at": 1782740693,
    },
    "secondary": {
        "used_percent": 40.0,
        "window_minutes": 10080,
        "resets_at": 1783297723,
    },
}


def claude_usage_from_shape(shape, inp, out, cread, ccreate):
    """Build a usage dict with the sampled key layout but consistent fixed
    values. Unknown keys are dropped (privacy-first)."""
    u = {}
    for k in shape:
        if k == "input_tokens":
            u[k] = inp
        elif k == "output_tokens":
            u[k] = out
        elif k == "cache_read_input_tokens":
            u[k] = cread
        elif k == "cache_creation_input_tokens":
            u[k] = ccreate
        elif k == "service_tier":
            u[k] = "standard"
        elif k == "inference_geo":
            u[k] = "not_available"
        elif k == "speed":
            u[k] = "standard"
        elif k == "cache_creation":
            u[k] = {
                "ephemeral_5m_input_tokens": ccreate,
                "ephemeral_1h_input_tokens": 0,
            }
        elif k == "server_tool_use":
            u[k] = {"web_search_requests": 0, "web_fetch_requests": 0}
        elif k == "iterations":
            u[k] = [
                {
                    "type": "message",
                    "input_tokens": inp,
                    "output_tokens": out,
                    "cache_read_input_tokens": cread,
                    "cache_creation_input_tokens": ccreate,
                    "cache_creation": {
                        "ephemeral_5m_input_tokens": ccreate,
                        "ephemeral_1h_input_tokens": 0,
                    },
                }
            ]
        # anything else: drop
    return u


def claude_record(template, ts, session_id, req_id, msg_id, usage, model=None):
    msg_t = template.get("message") or {}
    return {
        "type": "assistant",
        "timestamp": ts,
        "sessionId": session_id,
        "requestId": req_id,
        "message": {
            "id": msg_id,
            "model": model or msg_t.get("model") or "claude-fable-5",
            "usage": usage,
        },
    }


def codex_usage(inp, cached, out, reasoning, total):
    return {
        "input_tokens": inp,
        "cached_input_tokens": cached,
        "output_tokens": out,
        "reasoning_output_tokens": reasoning,
        "total_tokens": total,
    }


def codex_token_count(ts, last, total, rate_limits=None):
    rl = dict(FIXED_RATE_LIMITS) if rate_limits is None else rate_limits
    return {
        "timestamp": ts,
        "type": "event_msg",
        "payload": {
            "type": "token_count",
            "info": {"total_token_usage": total, "last_token_usage": last},
            "rate_limits": rl,
        },
    }


def codex_turn_context(ts, model="gpt-5.5"):
    return {"timestamp": ts, "type": "turn_context", "payload": {"model": model}}


def write_jsonl(path, lines):
    """lines: list of dicts (json-encoded) or raw strings (written as-is)."""
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w") as fh:
        for item in lines:
            if isinstance(item, str):
                fh.write(item + "\n")
            else:
                fh.write(json.dumps(item) + "\n")
    return path


def generate():
    written = []

    # ---- Claude ----------------------------------------------------------
    claude_t = sample_claude_assistant()
    usage_shape = list(((claude_t.get("message") or {}).get("usage") or {}).keys())

    def cu(i, o, cr, cc):
        return claude_usage_from_shape(usage_shape, i, o, cr, cc)

    cdir = os.path.join(FIXTURES_DIR, "claude")

    # basic.jsonl: AC3 -- 3 assistant records input 100/200/300, today input
    # sum 600. Cache record kept in a separate file so the AC3 sum stays 600.
    written.append(write_jsonl(os.path.join(cdir, "basic.jsonl"), [
        claude_record(claude_t, "2026-07-09T01:00:00.000Z", CLAUDE_SESSION_A,
                      "req_fixture_basic_01", "msg_fixture_basic_01",
                      cu(100, 10, 0, 0)),
        claude_record(claude_t, "2026-07-09T01:05:00.000Z", CLAUDE_SESSION_A,
                      "req_fixture_basic_02", "msg_fixture_basic_02",
                      cu(200, 20, 0, 0)),
        claude_record(claude_t, "2026-07-09T01:10:00.000Z", CLAUDE_SESSION_A,
                      "req_fixture_basic_03", "msg_fixture_basic_03",
                      cu(300, 30, 0, 0)),
    ]))

    # cache_record.jsonl: Test case (4) -- 4 disjoint categories, total 1350.
    written.append(write_jsonl(os.path.join(cdir, "cache_record.jsonl"), [
        claude_record(claude_t, "2026-07-09T01:20:00.000Z", CLAUDE_SESSION_A,
                      "req_fixture_cache_01", "msg_fixture_cache_01",
                      cu(100, 50, 1000, 200)),
    ]))

    # legacy_missing_cache_fields.jsonl: old-style usage without cache fields.
    written.append(write_jsonl(
        os.path.join(cdir, "legacy_missing_cache_fields.jsonl"), [
            claude_record(claude_t, "2026-07-09T01:30:00.000Z", CLAUDE_SESSION_A,
                          "req_fixture_legacy_01", "msg_fixture_legacy_01",
                          {"input_tokens": 40, "output_tokens": 5,
                           "service_tier": "standard"}),
        ]))

    # malformed.jsonl: broken JSON + non-assistant + one valid assistant.
    written.append(write_jsonl(os.path.join(cdir, "malformed.jsonl"), [
        '{"type": "assistant", "timestamp": "2026-07-09T01:59:00.000Z", "message": {',
        {"type": "user", "timestamp": "2026-07-09T01:59:30.000Z",
         "sessionId": CLAUDE_SESSION_A},
        'this is not json at all {{{',
        claude_record(claude_t, "2026-07-09T02:00:00.000Z", CLAUDE_SESSION_A,
                      "req_fixture_malformed_01", "msg_fixture_malformed_01",
                      cu(42, 7, 0, 0)),
    ]))

    # resume_duplicate_a/b.jsonl: T1(h) CONFIRMED -- resumed sessions copy
    # earlier assistant records (same message.id + requestId, identical usage)
    # into the new session file. Real copies KEEP the original sessionId
    # (verified on 200 sampled cross-file duplicate keys), so the copies below
    # carry CLAUDE_SESSION_A while living in file "b". b = copies + 1 new.
    dup1 = claude_record(claude_t, "2026-07-09T02:10:00.000Z", CLAUDE_SESSION_A,
                         "req_fixture_dup_01", "msg_fixture_dup_01",
                         cu(100, 10, 0, 0))
    dup2 = claude_record(claude_t, "2026-07-09T02:15:00.000Z", CLAUDE_SESSION_A,
                         "req_fixture_dup_02", "msg_fixture_dup_02",
                         cu(200, 20, 0, 0))
    written.append(write_jsonl(os.path.join(cdir, "resume_duplicate_a.jsonl"),
                               [dup1, dup2]))
    new_b = claude_record(claude_t, "2026-07-09T02:30:00.000Z", CLAUDE_SESSION_B,
                          "req_fixture_dup_03", "msg_fixture_dup_03",
                          cu(300, 30, 0, 0))
    written.append(write_jsonl(os.path.join(cdir, "resume_duplicate_b.jsonl"),
                               [dict(dup1), dict(dup2), new_b]))

    # ---- Codex -----------------------------------------------------------
    # Sample real structures once (validates the projected shapes exist).
    sample_codex("token_count")
    sample_codex("turn_context")
    info_null_t = sample_codex("info_null")

    xdir = os.path.join(FIXTURES_DIR, "codex")

    # basic_session.jsonl: Test case (2) values. last totals 100/150/150,
    # cumulative total_token_usage 100 -> 250 -> 400.
    written.append(write_jsonl(os.path.join(xdir, "basic_session.jsonl"), [
        codex_turn_context("2026-07-09T03:00:00.000Z"),
        codex_token_count("2026-07-09T03:01:00.000Z",
                          codex_usage(80, 30, 20, 5, 100),
                          codex_usage(80, 30, 20, 5, 100)),
        codex_token_count("2026-07-09T03:02:00.000Z",
                          codex_usage(100, 60, 50, 10, 150),
                          codex_usage(180, 90, 70, 15, 250)),
        codex_token_count("2026-07-09T03:03:00.000Z",
                          codex_usage(110, 90, 40, 8, 150),
                          codex_usage(290, 180, 110, 23, 400)),
    ]))

    # subset_semantics.jsonl: Test case (1) measured-style record.
    written.append(write_jsonl(os.path.join(xdir, "subset_semantics.jsonl"), [
        codex_turn_context("2026-07-09T03:10:00.000Z"),
        codex_token_count("2026-07-09T03:11:00.000Z",
                          codex_usage(20315, 4992, 902, 460, 21217),
                          codex_usage(20315, 4992, 902, 460, 21217)),
    ]))

    # subset_violation.jsonl: Test case (6b) -- cached > input violation + one
    # normal record.
    written.append(write_jsonl(os.path.join(xdir, "subset_violation.jsonl"), [
        codex_turn_context("2026-07-09T03:20:00.000Z"),
        codex_token_count("2026-07-09T03:21:00.000Z",
                          codex_usage(50, 120, 10, 0, 60),
                          codex_usage(50, 120, 10, 0, 60)),
        codex_token_count("2026-07-09T03:22:00.000Z",
                          codex_usage(50, 20, 10, 0, 60),
                          codex_usage(100, 140, 20, 0, 120)),
    ]))

    # repeated_total.jsonl: T1(a) OBSERVED PATTERN -- duplicate token_count
    # emissions repeat a nonzero last_token_usage while total_token_usage is
    # unchanged (12,658/40,468 real events; cause of all 47 naive-sum
    # mismatches). Correct aggregate skips the unchanged-total event:
    # 100 + 150 = 250 == final total. Naive sum-of-last = 350 (adversarial).
    written.append(write_jsonl(os.path.join(xdir, "repeated_total.jsonl"), [
        codex_turn_context("2026-07-09T03:25:00.000Z"),
        codex_token_count("2026-07-09T03:26:00.000Z",
                          codex_usage(80, 30, 20, 5, 100),
                          codex_usage(80, 30, 20, 5, 100)),
        codex_token_count("2026-07-09T03:26:30.000Z",
                          codex_usage(80, 30, 20, 5, 100),
                          codex_usage(80, 30, 20, 5, 100)),
        codex_token_count("2026-07-09T03:27:00.000Z",
                          codex_usage(100, 60, 50, 10, 150),
                          codex_usage(180, 90, 70, 15, 250)),
    ]))

    # total_reset.jsonl: T1(a) OBSERVED PATTERN -- total_token_usage decreases
    # mid-session (4/149 real files). The post-reset event is a fresh baseline
    # and must still be counted: correct usage = 100 + 150 + 50 = 300; final
    # cumulative total (50) can NOT be used as the session total (adversarial).
    written.append(write_jsonl(os.path.join(xdir, "total_reset.jsonl"), [
        codex_turn_context("2026-07-09T03:28:00.000Z"),
        codex_token_count("2026-07-09T03:28:10.000Z",
                          codex_usage(80, 0, 20, 0, 100),
                          codex_usage(80, 0, 20, 0, 100)),
        codex_token_count("2026-07-09T03:28:20.000Z",
                          codex_usage(100, 0, 50, 0, 150),
                          codex_usage(180, 0, 70, 0, 250)),
        codex_token_count("2026-07-09T03:28:30.000Z",
                          codex_usage(40, 0, 10, 0, 50),
                          codex_usage(40, 0, 10, 0, 50)),
    ]))

    # rate_limits.jsonl: Test case (6) -- Measured expected values.
    written.append(write_jsonl(os.path.join(xdir, "rate_limits.jsonl"), [
        codex_token_count("2026-07-09T03:30:00.000Z",
                          codex_usage(10, 0, 5, 0, 15),
                          codex_usage(10, 0, 5, 0, 15)),
    ]))

    # info_null.jsonl: real projected info:null record structure, fixed
    # rate_limits values.
    rec = json.loads(json.dumps(info_null_t))
    rec["timestamp"] = "2026-07-09T03:40:00.000Z"
    rl = rec["payload"].get("rate_limits")
    if isinstance(rl, dict):
        for k, v in FIXED_RATE_LIMITS.items():
            if k in rl:
                rl[k] = v
    else:
        rec["payload"]["rate_limits"] = dict(FIXED_RATE_LIMITS)
    written.append(write_jsonl(os.path.join(xdir, "info_null.jsonl"), [rec]))

    # malformed.jsonl: truncated line + unknown payload.type + valid record.
    written.append(write_jsonl(os.path.join(xdir, "malformed.jsonl"), [
        '{"timestamp": "2026-07-09T02:59:00.000Z", "type": "event_msg", "payload": {"type": "token_c',
        {"timestamp": "2026-07-09T03:50:00.000Z", "type": "event_msg",
         "payload": {"type": "totally_unknown_event"}},
        codex_token_count("2026-07-09T03:51:00.000Z",
                          codex_usage(10, 0, 5, 0, 15),
                          codex_usage(10, 0, 5, 0, 15)),
    ]))

    # dup/: Test case (3)/(3b). Same uuid in both trees (byte-identical), plus
    # one archived-only uuid (completed sessions->archived move).
    dup_lines = [
        codex_turn_context("2026-01-01T00:00:00.000Z"),
        codex_token_count("2026-01-01T00:01:00.000Z",
                          codex_usage(40, 10, 10, 0, 50),
                          codex_usage(40, 10, 10, 0, 50)),
        codex_token_count("2026-01-01T00:02:00.000Z",
                          codex_usage(60, 30, 20, 0, 80),
                          codex_usage(100, 40, 30, 0, 130)),
    ]
    dup_name = f"rollout-2026-01-01T00-00-00-{CODEX_DUP_UUID}.jsonl"
    written.append(write_jsonl(
        os.path.join(xdir, "dup", "sessions", "2026", "01", "01", dup_name),
        dup_lines))
    # archived tree is flat on this machine (verified) -- mirror that.
    written.append(write_jsonl(
        os.path.join(xdir, "dup", "archived_sessions", dup_name), dup_lines))

    moved_name = f"rollout-2026-01-02T00-00-00-{CODEX_MOVED_UUID}.jsonl"
    written.append(write_jsonl(
        os.path.join(xdir, "dup", "archived_sessions", moved_name), [
            codex_turn_context("2026-01-02T00:00:00.000Z"),
            codex_token_count("2026-01-02T00:01:00.000Z",
                              codex_usage(30, 0, 15, 0, 45),
                              codex_usage(30, 0, 15, 0, 45)),
        ]))

    for p in written:
        print("wrote", os.path.relpath(p, REPO_ROOT))
    return 0


# --------------------------------------------------------------------------
# --verify-scrub
# --------------------------------------------------------------------------

def verify_scrub():
    violations = []
    files = sorted(glob.glob(os.path.join(FIXTURES_DIR, "**", "*.jsonl"),
                             recursive=True))
    if not files:
        print("verify-scrub: no fixture files found", file=sys.stderr)
        return 1
    for f in files:
        rel = os.path.relpath(f, FIXTURES_DIR)
        if rel.split(os.sep)[0] == "claude":
            allowed = CLAUDE_ALLOWED
        elif rel.split(os.sep)[0] == "codex":
            allowed = CODEX_ALLOWED
        else:
            violations.append((rel, 0, "<file outside claude/ or codex/>"))
            continue
        with open(f, "r") as fh:
            for i, line in enumerate(fh, 1):
                line = line.strip()
                if not line:
                    continue
                try:
                    rec = json.loads(line)
                except json.JSONDecodeError:
                    if line not in KNOWN_MALFORMED_LINES:
                        violations.append((rel, i, "<unparseable non-constant line>"))
                    continue
                for path in _walk_paths(rec):
                    if not _match(path, allowed):
                        violations.append((rel, i, path))
    if violations:
        print(f"verify-scrub FAILED: {len(violations)} violation(s)")
        for rel, i, path in violations:
            print(f"  {rel}:{i}: disallowed key path: {path}")
        return 1
    print(f"verify-scrub OK: {len(files)} files, allowlist-only keys")
    return 0


# --------------------------------------------------------------------------
# --survey: one-shot real-log verification for README (a)-(h)
# --------------------------------------------------------------------------

def survey_codex(limit=None):
    files = sorted(
        glob.glob(os.path.join(CODEX_SESSIONS, "**", "*.jsonl"), recursive=True)
    ) + sorted(
        glob.glob(os.path.join(CODEX_ARCHIVED, "**", "*.jsonl"), recursive=True)
    )
    if limit:
        files = files[:limit]
    n_files = len(files)
    stats = {
        "files": n_files,
        "files_with_token_count": 0,
        "sum_last_eq_final_total": 0,
        "sum_last_ne_final_total": 0,
        "files_with_total_reset": 0,
        "token_count_records": 0,
        "info_null_records": 0,
        "f_total_eq_in_plus_out_last": 0,
        "f_cached_subset_last": 0,
        "f_reasoning_subset_last": 0,
        "f_violations_last": 0,
        "f_total_eq_in_plus_out_cum": 0,
        "f_violations_cum": 0,
        "records_without_rate_limits": 0,
        "files_with_turn_context": 0,
        "files_with_model": 0,
    }
    rate_limit_shapes = {}
    model_names = {}
    mismatch_details = []  # (basename, sum_last, final_total, resets)

    for path in files:
        sum_last = 0
        final_total = None
        prev_total = None
        resets = 0
        n_tc = 0
        has_turn_context = False
        has_model = False
        with open(path, "rb") as fh:
            for raw in fh:
                if b'"token_count"' not in raw and b'"turn_context"' not in raw:
                    continue
                try:
                    rec = json.loads(raw)
                except (json.JSONDecodeError, UnicodeDecodeError):
                    continue
                p = rec.get("payload")
                if not isinstance(p, dict):
                    continue
                if rec.get("type") == "turn_context":
                    has_turn_context = True
                    m = p.get("model")
                    if m:
                        has_model = True
                        model_names[m] = model_names.get(m, 0) + 1
                    continue
                if p.get("type") != "token_count":
                    continue
                stats["token_count_records"] += 1
                rl = p.get("rate_limits")
                if rl is None:
                    stats["records_without_rate_limits"] += 1
                elif isinstance(rl, dict):
                    shape = json.dumps(sorted(rl.keys()))
                    rate_limit_shapes[shape] = rate_limit_shapes.get(shape, 0) + 1
                info = p.get("info")
                if info is None:
                    stats["info_null_records"] += 1
                    continue
                if not isinstance(info, dict):
                    continue
                last = info.get("last_token_usage") or {}
                cum = info.get("total_token_usage") or {}
                n_tc += 1
                lt = last.get("total_tokens", 0)
                sum_last += lt
                ct = cum.get("total_tokens", 0)
                final_total = ct
                if prev_total is not None and ct < prev_total:
                    resets += 1
                prev_total = ct
                # (f) subset semantics on last_token_usage
                ok_total = lt == last.get("input_tokens", 0) + last.get("output_tokens", 0)
                ok_cached = last.get("cached_input_tokens", 0) <= last.get("input_tokens", 0)
                ok_reason = last.get("reasoning_output_tokens", 0) <= last.get("output_tokens", 0)
                stats["f_total_eq_in_plus_out_last"] += ok_total
                stats["f_cached_subset_last"] += ok_cached
                stats["f_reasoning_subset_last"] += ok_reason
                if not (ok_total and ok_cached and ok_reason):
                    stats["f_violations_last"] += 1
                ok_cum = ct == cum.get("input_tokens", 0) + cum.get("output_tokens", 0)
                stats["f_total_eq_in_plus_out_cum"] += ok_cum
                if not ok_cum:
                    stats["f_violations_cum"] += 1
        if n_tc:
            stats["files_with_token_count"] += 1
            if resets:
                stats["files_with_total_reset"] += 1
            if sum_last == final_total:
                stats["sum_last_eq_final_total"] += 1
            else:
                stats["sum_last_ne_final_total"] += 1
                mismatch_details.append(
                    (os.path.basename(path), sum_last, final_total, resets)
                )
        if has_turn_context:
            stats["files_with_turn_context"] += 1
        if has_model:
            stats["files_with_model"] += 1

    print("== codex survey (a)/(c)/(e)/(f) ==")
    for k, v in stats.items():
        print(f"  {k}: {v}")
    print("  rate_limits key-set shapes:")
    for shape, n in sorted(rate_limit_shapes.items(), key=lambda x: -x[1]):
        print(f"    n={n}: {shape}")
    print(f"  turn_context model names: {json.dumps(model_names)}")
    print(f"  (a) mismatch details ({len(mismatch_details)}):")
    for name, s, t, r in mismatch_details:
        print(f"    {name}: sum_last={s} final_total={t} resets={r}")
    return stats


def survey_claude():
    files = _claude_files()
    shapes = {}
    id_map = {}
    total_assistant = 0
    for f in files:
        for rec in _iter_records(f):
            if rec.get("type") != "assistant":
                continue
            total_assistant += 1
            msg = rec.get("message") or {}
            u = msg.get("usage")
            if isinstance(u, dict):
                shapes[json.dumps(sorted(u.keys()))] = (
                    shapes.get(json.dumps(sorted(u.keys())), 0) + 1
                )
            key = (msg.get("id"), rec.get("requestId"))
            id_map.setdefault(key, []).append(
                (f, json.dumps(u, sort_keys=True))
            )
    multi = {k: v for k, v in id_map.items() if len(v) > 1}
    cross_file = {
        k: v for k, v in multi.items() if len(set(x[0] for x in v)) > 1
    }
    within_file = {
        k: v for k, v in multi.items() if len(set(x[0] for x in v)) == 1
    }
    identical_usage = sum(
        1 for v in multi.values() if len(set(x[1] for x in v)) == 1
    )
    varying_usage = len(multi) - identical_usage
    print("== claude survey (b)/(h) ==")
    print(f"  files: {len(files)}, assistant records: {total_assistant}")
    print("  usage key-set variants:")
    for shape, n in sorted(shapes.items(), key=lambda x: -x[1]):
        print(f"    n={n}: {shape}")
    print(f"  unique (message.id, requestId) keys: {len(id_map)}")
    print(f"  keys with >1 record: {len(multi)}"
          f" (cross-file: {len(cross_file)}, within-file-only: {len(within_file)})")
    print(f"  duplicated keys with byte-identical usage: {identical_usage}, "
          f"varying usage: {varying_usage}")


def survey(limit=None):
    survey_claude()
    print()
    survey_codex(limit=limit)
    return 0


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--verify-scrub", action="store_true")
    ap.add_argument("--survey", action="store_true")
    ap.add_argument("--survey-limit", type=int, default=None,
                    help="limit codex survey to first N files (testing only)")
    args = ap.parse_args()
    if args.verify_scrub:
        sys.exit(verify_scrub())
    if args.survey:
        sys.exit(survey(limit=args.survey_limit))
    sys.exit(generate())


if __name__ == "__main__":
    main()
