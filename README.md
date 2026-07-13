# meterly

macOS 메뉴바(트레이)에 상주하며 로컬에서 사용 중인 AI 코딩 도구(Claude Code, Codex CLI)의
토큰 사용량을 보여주는 앱. **네트워크를 전혀 사용하지 않고**, 각 도구가 로컬 디스크에
남기는 로그 파일만 파싱합니다.

## 기능

- **트레이 상주** — 아이콘 옆에 오늘 총 토큰(또는 환산 비용) 상시 표시, 3분마다 자동 갱신
- **팝오버** (트레이 좌클릭) — 도구별 오늘 토큰, API 환산 비용, 캐시 절약액, 한도 상태, 7일 스파크라인
- **대시보드** — 일(30일)/주(12주)/월(6개월) 추이, 토큰 구성(입력·출력·캐시), 비용 추이,
  모델별 비교, 요일×시간 사용 패턴 히트맵
- **한도 알림** — Codex 사용률이 창의 80%/95%를 넘으면 macOS 알림
- **내보내기** — 현재 범위 집계를 CSV/JSON으로 `~/Downloads`에 저장
- **트레이 메뉴** (우클릭) — 대시보드 열기 / 지금 새로고침 / 트레이 표시(토큰·비용·아이콘만) /
  로그인 시 자동 시작 / 종료

## 표기 의미

| 라벨 | 의미 |
|---|---|
| **API 환산** | 구독 요금이 아니라, 사용 토큰을 API 정가로 환산한 참고 금액 |
| **추정** | Claude 한도 — 로컬에 공식 데이터가 없어 5시간 롤링 창으로 추정한 값 |
| **로그 기준** | Codex 한도 — 로그에 기록된 실측값 (사용률 %, 리셋 시각) |
| **캐시로 $X 절약** | 캐시 읽기 토큰을 정가 입력 요율과의 차액으로 환산한 절약액 |

## 설치

### 릴리즈 다운로드

[Releases](https://github.com/miss0110/meterly/releases)에서 받아 설치:

- macOS: `.dmg` (Apple Silicon)
- Windows: `.msi` 또는 `setup.exe`

서명/공증이 없으므로 macOS에서 첫 실행 시 Gatekeeper 경고가 뜨면
**시스템 설정 → 개인정보 보호 및 보안 → "그래도 열기"** 로 허용하세요.

### 소스에서 빌드

요구사항: Rust(stable), Node 22+

```sh
npm ci
npm run tauri build
# 산출물: src-tauri/target/release/bundle/
```

macOS에서 응용 프로그램 폴더로 설치:

```sh
ditto src-tauri/target/release/bundle/macos/meterly.app /Applications/meterly.app
```

## 데이터 소스와 프라이버시

| 도구 | 읽는 위치 | 방식 |
|---|---|---|
| Claude Code | `~/.claude/projects/**/*.jsonl` | 세션 트랜스크립트의 usage 블록 집계 (resume 중복 제거) |
| Codex CLI | `~/.codex/sessions/**`, `~/.codex/archived_sessions/**` | `token_count` 이벤트 집계 (누적치 변화 기준, 세션 uuid 중복 제거) |

- 앱은 **어떤 데이터도 외부로 전송하지 않습니다** (오프라인 완전 동작)
- 저장하는 것은 토큰 수·날짜·모델명·세션 id·파일 커서뿐 — **대화 내용은 캐시에 저장하지 않습니다**
  (캐시: `~/Library/Application Support/com.meterly.app/cache-v1.json`)
- 로그 파일은 읽기 전용으로만 접근합니다
- CLI 업데이트로 로그 포맷이 바뀌면 해당 도구만 "⚠ 읽기오류"로 표시되고 나머지는 계속 동작합니다

## 정확도에 대해

파서는 이 저장소의 `fixtures/`(실로그에서 민감 정보를 제거하고 추출한 샘플)를 입력으로 한
39개 테스트로 검증되며, 특히 이중 집계를 유발하는 실측 함정들을 방어합니다:

- Codex `last_token_usage`의 중복 방출 이벤트 (누적 total 불변 → skip)
- `cached_input ⊆ input` 부분집합 의미론 (그대로 더하면 과대 집계)
- `sessions/` → `archived_sessions/` 파일 이동 (uuid 키 커서로 무연산 처리)
- Claude resume 세션의 usage 레코드 복사 (message id 기반 전역 dedup)

검증 상세: [`fixtures/README.md`](fixtures/README.md)

## 개발

```sh
npm run tauri dev                                     # 개발 실행
cargo test --manifest-path src-tauri/Cargo.toml       # 백엔드 테스트
npm run check                                         # svelte/ts 타입체크
python3 scripts/capture_fixtures.py --verify-scrub    # 픽스처 민감정보 검사
```

새 도구 파서 추가: `src-tauri/src/sources/`에 `UsageSource` trait 구현 파일 1개 +
`sources/mod.rs`의 `registry()`에 1줄 등록.

가격표는 `src-tauri/src/pricing.rs`의 상수 테이블 — 모델 요율이 바뀌면 여기를 수정하세요.

## 알려진 제한

- 비용은 API 정가 환산 참고치이며 구독 실지출이 아닙니다
- Claude 한도는 추정치입니다 (플랜 상한을 로컬에서 알 수 없어 잔여 %는 표시하지 않음)
- Windows 빌드는 CI에서 생성되지만 실기기 검증은 macOS 대비 제한적입니다
  (Windows는 트레이 텍스트 미지원 → 툴팁으로 표시)
- "오늘" 경계는 시스템 로컬 타임존 자정 기준 — UTC 기준 도구와 수치가 다를 수 있습니다
