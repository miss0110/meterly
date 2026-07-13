# fixtures/ — T1 포맷 검증 결과·증거 기록

생성: `python3 scripts/capture_fixtures.py` (allowlist 투영 — 파서가 필요로 하는
키만 방출, 대화 본문/프롬프트/cwd/도구 입출력/thinking은 구조적으로 유출 불가).
검증: `python3 scripts/capture_fixtures.py --verify-scrub` (allowlist 외 키 발견
시 비영 종료). 전 수치는 이 머신 실로그 전수 스캔 실측값 (스캔일 2026-07-13).

스캔 모수: Claude `~/.claude/projects/**/*.jsonl` 106 files / assistant 10,643건.
Codex `~/.codex/sessions/**` + `~/.codex/archived_sessions/**` 170 files (sessions
144 + archived 26), token_count 40,565건 (info 보유 40,468 + `info: null` 97).

---

## (a) Codex Σ last_token_usage.total == 세션 최종 total_token_usage.total

**naive 성립: 102/149 파일 (68.5%). 불일치 47 파일 — 전건 Σlast > 최종 total.**
token_count 없는 파일 21개(전부 2025-09 구형 포맷 — 아래 참고)는 모수 제외.

불일치 원인 실측 (기전 확정):
- **중복 방출 패턴**: 직전 이벤트와 `total_token_usage`가 완전히 동일한(누적
  불변) token_count가 0이 아닌 `last_token_usage`를 반복 방출 — 실측 12,658/
  40,468 이벤트 (31.3%). naive Σlast는 이 반복분을 이중 가산한다.
- **total 리셋(누적 감소)**: 4/149 파일에서 관측 (resume/compact 의심). 리셋
  후 누적이 새 베이스라인에서 재시작하므로 최종 total은 세션 사용량이 아니다.

**처리 규칙 검증 (T5 입력 — 확정 권고)**: "직전 이벤트 대비
`total_token_usage.total_tokens`가 변한 이벤트의 `last_token_usage`만 합산
(불변이면 중복 방출로 skip; 감소(리셋)는 새 베이스라인으로 카운트 지속)".
- 리셋 없는 145 파일 중 **144 파일에서 정확 등식 성립 (99.3%)**.
- 잔여 1 파일: rule 합 85,719,644 vs 최종 total 85,711,213 (차이 8,431,
  0.0098%) — 단일 파일 미세 잔차, 원인 미상. skip+Partial 백스톱 범위.
- 리셋 4 파일: rule 합 ≥ 최종 total 전건 성립 (리셋으로 최종 total이 세션
  합보다 작아지는 방향 — 예상과 일치).
- 이 규칙의 합은 "누적 total 증가분 차분 합"과 진단 파일 3건에서 정확히 일치
  (두 방식 등가) — 계획 (a)의 "불일치가 일반적이면 total 차분 방식 전환"
  fallback과 동일한 결론이며, 정규화 표(레코드 필드 매핑)는 변경 불필요.

패턴 픽스처: `codex/repeated_total.jsonl` (중복 방출 — 올바른 합 250, naive
350), `codex/total_reset.jsonl` (리셋 — 올바른 합 300, 최종 total 50 사용 금지).

구형 포맷 참고: 2025-09 초기 rollout 파일들은 `event_msg`/`payload` 봉투 없이
평면 레코드(`record_type` 키)만 있고 **token_count 이벤트가 전무** — 토큰
이벤트 0건으로 자연 처리됨 (파서는 미지 레코드 무시, health Ok).

## (b) Claude usage 필드 변형 + input_tokens/cache 서로소

전 106 파일 assistant 10,643건의 usage 키 집합 — **변형 2종만 존재, 둘 다
cache 필드 포함**:

| n | 키 집합 |
|---|---|
| 10,320 | cache_creation, cache_creation_input_tokens, cache_read_input_tokens, inference_geo, input_tokens, iterations, output_tokens, server_tool_use, service_tier, speed |
| 323 | cache_creation, cache_creation_input_tokens, cache_read_input_tokens, inference_geo, input_tokens, output_tokens, service_tier |

- 4개 토큰 필드(input/output/cache_read/cache_creation)는 두 변형 공통 —
  정규화 표의 Claude 측 매핑에 결손 없음.
- **서로소 판단**: usage에 total 필드가 없어 로그만으로 산술 검증은 불가
  (구조적 확인만 가능). 실측 구조 증거: input_tokens가 소값(예: 8668)인
  레코드에서 cache_read(25,287)/cache_creation(4,007)이 훨씬 큼 — 포함 관계라면
  불가능한 분포. Anthropic API 문서상 input_tokens는 캐시 토큰 미포함(서로소)
  — **이를 전제로 고정** (계획 지시대로 전제 기록). 붕괴 증거 없음 → 에스컬
  레이션 불요.
- cache 필드 없는 구형 변형은 현 로그에 부재. `legacy_missing_cache_fields.jsonl`
  은 스키마 드리프트 방어용 합성 픽스처임을 명시.

## (c) rate_limits 페이로드 실물

- 최신 실물 필드: `limit_id`, `limit_name`, `primary{used_percent,
  window_minutes, resets_at}`, `secondary{동일}`, `credits`, `individual_limit`,
  `plan_type`, `rate_limit_reached_type`.
- 키 집합 변형 6종 실측 (CLI 버전에 따른 필드 증식 — 최소형은
  `{primary, secondary}`만): n=22,446 / 8,621 / 3,973 / 2,590 / 1,950 / 978.
  **파서는 `primary`/`secondary` 외 필드를 optional로 취급해야 함.**
- **`resets_at`은 epoch 초 확정**: 실측 1782740693 → 2026-06-29T13:44:53Z,
  1783297723 → 2026-07-06T00:28:43Z (파일 mtime 시기와 정합).
- `info: null` + rate_limits만 있는 token_count 실재: **97건**. 픽스처
  `codex/info_null.jsonl` (실물 투영 구조).
- rate_limits 자체가 없는 token_count도 7건 존재 — rate_limits는 optional.

## (d) 쓰기 중 파일 마지막 줄 불완전 JSON

최근 수정 상위 3 Codex + 3 Claude 파일의 마지막 줄 `json.loads` 전건 성공,
전 파일 개행 종료 (검사 시점 활성 쓰기 없음). **불완전 줄은 이번 검사에서
미관측**이나, 폴링 중 활성 쓰기와 겹칠 수 있으므로 T6의 "불완전 마지막 줄 →
커서 유지" 방어는 그대로 필요 (관측 부재 ≠ 발생 불가).

## (e) Codex 모델 귀속

- `token_count` 이벤트에는 모델명 없음 (`info`에는 usage 2종 +
  `model_context_window`뿐).
- `session_meta.payload`에도 모델명 없음 (`model_provider`("openai")만).
- **`turn_context`(top-level type) 레코드의 `payload.model`에 실물 모델명
  존재** (예: "gpt-5.5", "gpt-5.1-codex-max" 등 7종 실측). 155/170 파일에
  turn_context 존재 (부재 15개는 구형/무이벤트 파일).
- **귀속 규칙 확정: 같은 세션 파일 내 해당 token_count보다 앞선 가장 최근
  turn_context의 `payload.model`. turn_context가 전무한 파일은 "unknown".**
- allowlist에 `payload.model` 추가 완료. 픽스처의 세션류 파일에 turn_context
  레코드 포함.

## (f) Codex 부분집합 의미론 전 세션 검증

info 보유 40,468 이벤트의 `last_token_usage` 기준:

| 불변식 | 성립 | 비율 |
|---|---|---|
| `cached_input_tokens ⊆ input_tokens` | 40,468/40,468 | **100%** |
| `reasoning_output_tokens ⊆ output_tokens` | 40,468/40,468 | **100%** |
| `total == input + output` | 40,039/40,468 | 98.94% (위반 429) |
| (참고) 누적 `total == input + output` | 40,468/40,468 | **100%** |

- **위반 429건 중 428건은 (a)의 중복 방출(누적 불변) 이벤트에서 발생** —
  (a) 처리 규칙이 skip하는 레코드라 집계에 도달하지 않음. 규칙 적용 후 잔여
  위반은 전 코퍼스에서 **1건 (0.004%)** → C2 skip+Partial 정책으로 흡수.
- cached>input 위반은 실로그 미관측 — `codex/subset_violation.jsonl`은 C2
  언더플로 방어(Test case 6b) 검증용 합성 픽스처.
- 실측 스타일 레코드 픽스처: `codex/subset_semantics.jsonl` (input 20315,
  cached 4992, output 902, reasoning 460, total 21217; 계획 명시값).
- 의미론 전제 붕괴 없음 → 에스컬레이션 트리거 (1) 미해당.

## (g) archived_sessions 실태

- sessions 144 uuid, archived 26 uuid. **중복 22 uuid / archived 단독 4 uuid**
  (사전 확인값과 일치 — sessions→archived 이동 실재, C1 근거).
- archived 단독 uuid: 019d9048-ec2c-7761-85c2-a9b9f7cd0ed9,
  019d9058-84f7-7fb1-97c7-0a2eaecd609b, 019d9059-2652-7491-9e2e-c15f171b7975,
  019e4133-f0f4-7cb2-b5be-6efc3024f7cf.
- 중복 22쌍 바이트 비교 (전량 cmp/prefix 검사): **바이트 동일 21쌍, archived가
  sessions의 prefix 1쌍** (archive 복사 후 sessions 쪽에 append 계속 — sessions
  우선 규칙이 정확히 이 케이스를 커버), **내용 분기(동일도 prefix도 아님) 0쌍**.
- **내용 분기 미발견 → sessions 우선 규칙 유지, 에스컬레이션 트리거 (6) 미해당.**
- 픽스처: `codex/dup/sessions/2026/01/01/rollout-...-0195aaaa-....jsonl` ==
  `codex/dup/archived_sessions/rollout-...-0195aaaa-....jsonl` (바이트 동일,
  cmp 확인) + archived 단독 `...-0195bbbb-....jsonl` (이동 완료 전이).
  실물과 동일하게 sessions는 YYYY/MM/DD 중첩, archived는 평면 배치.

## (h) Claude resume/continue 중복 — **확인됨 (B7 활성)**

- 전 106 파일 스캔: 고유 `(message.id, requestId)` 3,804키 중 **2,861키가
  복수 레코드** — 파일 간 1,427키, **파일 내 반복만도 1,434키** (스트리밍/
  멀티블록 기록). 중복키의 usage 바이트 동일 2,731 / 상이 130.
- 상이 130건의 변동은 **output_tokens만 증가** (input/cache 필드 불변) —
  스트리밍 중간 스냅샷과 최종본. → 중복키 처리 시 output_tokens 최대(=최종)
  레코드 채택 권고 (T4/T6 설계 입력).
- 파일 간 복사본은 **원본 sessionId를 유지**하고 (200키 표본 전건), requestId도
  전건 동일. 타임스탬프는 53/200만 동일 (재기록 시 갱신되는 경우 존재) →
  타임스탬프는 dedup 키로 부적합.
- **dedup 키 확정: `message.id + requestId`.** 파일 간뿐 아니라 **파일 내
  중복도 있으므로 dedup은 전역(global — 파일 경계 무관) seen_keys로 적용해야
  함.** naive 파일 합산은 assistant 레코드 기준 최대 2.8배 과대 집계.
- 픽스처: `claude/resume_duplicate_a.jsonl` (2건) +
  `claude/resume_duplicate_b.jsonl` (a의 복사 2건 — 실물처럼 원본 sessionId
  유지 — + 신규 1건). Test case (5)와 정합.

---

## 픽스처 목록·기대값

| 파일 | 용도 / 기대값 |
|---|---|
| claude/basic.jsonl | AC3: assistant 3건 input 100/200/300 (output 10/20/30, cache 0) → input 합 600 |
| claude/cache_record.jsonl | Test (4): input 100, output 50, cache_read 1000, cache_creation 200 → total 1350 (basic과 분리해 AC3 합 600 유지) |
| claude/legacy_missing_cache_fields.jsonl | cache 필드 없는 합성 구형 → cache 0 처리, health Ok |
| claude/malformed.jsonl | 불량 JSON 2줄 + user 1줄 + 정상 assistant 1건(input 42) → skip+Partial, 패닉 금지 |
| claude/resume_duplicate_a/b.jsonl | Test (5): dedup 후 == a 전체 + b 신규 1건 (input 600, output 60) |
| codex/basic_session.jsonl | Test (2): last total 100/150/150 (각각 그 레코드의 input+output), 누적 100→250→400; 정규화 집계 {input 110, cache_read 180, output 110}, 총합 400 |
| codex/subset_semantics.jsonl | Test (1): 정규화 {input 15323, cache_read 4992, output 902, cache_creation 0}, 총합 21217 |
| codex/subset_violation.jsonl | Test (6b): cached 120 > input 50 위반 1건 skip+Partial + 정상 1건(총합 60) 집계 |
| codex/repeated_total.jsonl | (a) 중복 방출 패턴: 누적 불변 이벤트 skip → 합 250 (naive 350은 오답) |
| codex/total_reset.jsonl | (a) 리셋 패턴: 합 100+150+50=300 (최종 누적 50 사용 금지) |
| codex/rate_limits.jsonl | Test (6): primary 25.0%/300분/1782740693(=2026-06-29T13:44:53Z), secondary 40.0%/10080분/1783297723 |
| codex/info_null.jsonl | Test (6) negative: info:null → 이벤트 미생성, rate_limits 스냅샷만 |
| codex/malformed.jsonl | 불량 줄 + 미지 payload.type + 정상 token_count 1건(총합 15) |
| codex/dup/** | Test (3)/(3b): 두 트리 동일 uuid(바이트 동일, 합 130 1회만) + archived 단독 uuid(합 45) |

합성 id 규약: Claude sessionId `aaaaaaaa-…-a`/`bbbbbbbb-…-b`, message.id
`msg_fixture_*`, requestId `req_fixture_*`; Codex 파일명 uuid
`0195aaaa-…-000000000001`(중복 쌍) / `0195bbbb-…-000000000002`(이동 전이).
실제 세션 id·프로젝트 경로 슬러그 미사용.
