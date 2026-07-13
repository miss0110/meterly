<div align="center">

# ⚡ meterly

**로컬 AI 코딩 도구 사용량을 메뉴바에서 한눈에**

Claude Code · Codex CLI의 토큰 사용량을 로컬 로그만 파싱해 보여주는 트레이 앱.
네트워크 요청 0회, API 키 불필요, 대화 내용 무저장.

[![Release](https://img.shields.io/github/v/release/miss0110/meterly?style=flat-square)](https://github.com/miss0110/meterly/releases)
[![Build](https://img.shields.io/github/actions/workflow/status/miss0110/meterly/build.yml?style=flat-square)](https://github.com/miss0110/meterly/actions)
![Platform](https://img.shields.io/badge/platform-macOS%20%7C%20Windows-blue?style=flat-square)
![Tauri](https://img.shields.io/badge/Tauri-v2-24C8DB?style=flat-square&logo=tauri&logoColor=white)
![Rust](https://img.shields.io/badge/Rust-backend-orange?style=flat-square&logo=rust)
![Local Only](https://img.shields.io/badge/data-100%25%20local-success?style=flat-square)

<!-- 스크린샷: docs/screenshot-dashboard.png / docs/screenshot-popover.png 파일을
     추가하고 아래 주석을 해제하면 표시됩니다.
<img src="docs/screenshot-dashboard.png" alt="meterly 대시보드 — 추이·토큰 구성·비용·모델 비교·히트맵" width="760" />
<img src="docs/screenshot-popover.png" alt="meterly 팝오버 — 도구별 오늘 사용량 요약" width="340" />
-->

</div>

---

## ✨ 기능

- 🖥️ **트레이 상주** — 오늘 총 토큰(또는 환산 비용)을 메뉴바에 상시 표시, 3분마다 자동 갱신
- 📊 **대시보드** — 일/주/월 추이, 토큰 구성(입력·출력·캐시), 비용 추이, 모델별 비교, 요일×시간 히트맵
- ⚡ **팝오버** — 트레이 클릭 한 번으로 도구별 오늘 요약 + 7일 스파크라인
- 🔔 **한도 알림** — Codex 사용률 80%/95% 도달 시 macOS 알림
- 💾 **CSV/JSON 내보내기** — 기간별 집계를 `~/Downloads`로
- 💸 **캐시 절약액** — 프롬프트 캐시 덕분에 아낀 금액을 환산 표시
- 🚀 **자동 시작** — 로그인 시 자동 실행 토글

## 📦 설치

**[Releases](https://github.com/miss0110/meterly/releases)** 에서 다운로드:

| 플랫폼 | 파일 |
|---|---|
| macOS (Apple Silicon) | `.dmg` |
| Windows | `.msi` / `setup.exe` |

> [!NOTE]
> 서명/공증이 없는 빌드입니다. macOS에서 첫 실행이 막히면
> **시스템 설정 → 개인정보 보호 및 보안 → "그래도 열기"** 로 허용하세요.

<details>
<summary><b>소스에서 빌드</b></summary>

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

</details>

## 🔒 프라이버시

**모든 것이 로컬입니다.** 이 앱은:

| | |
|---|---|
| 📖 읽는 것 | `~/.claude/projects/**/*.jsonl`, `~/.codex/{sessions,archived_sessions}/**/*.jsonl` (읽기 전용) |
| 💾 저장하는 것 | 토큰 수 · 날짜 · 모델명 · 세션 id · 파일 커서만 (`~/Library/Application Support/com.meterly.app/`) |
| 🚫 안 하는 것 | 네트워크 요청, 대화 내용 저장, API 키 사용 |

## 🏷️ 표기 의미

| 라벨 | 의미 |
|---|---|
| `API 환산` | 구독 요금이 아니라 사용 토큰을 API 정가로 환산한 **참고 금액** |
| `추정` | Claude 한도 — 공식 데이터가 로컬에 없어 5시간 롤링 창으로 **추정**한 값 |
| `로그 기준` | Codex 한도 — 로그에 기록된 **실측값** (사용률 %, 리셋 시각) |
| `캐시로 $X 절약` | 캐시 읽기 토큰 × (정가 입력 요율 − 캐시 요율) |

## 🎯 정확도

로컬 로그를 그냥 더하면 **수치가 틀립니다.** meterly의 파서는 실로그 분석으로 확인된
이중 집계 함정 4종을 방어하고, 스크럽된 실로그 픽스처 기반 39개 테스트로 검증됩니다:

1. **Codex 중복 방출 이벤트** (전체의 31%) — 누적 total이 변한 이벤트만 카운트
2. **캐시 토큰 부분집합 의미론** — `cached ⊆ input`을 그대로 더하면 과대 집계
3. **세션 파일 이동** (`sessions/` → `archived_sessions/`) — uuid 키 커서로 무연산 처리
4. **Claude resume 세션의 레코드 복사** — message id 기반 전역 dedup

<details>
<summary>검증 상세</summary>

각 함정에는 오답 값이 정확히 나오면 실패하는 adversarial 테스트가 있습니다
(예: 부분집합 무시 시 26,209 ≠ 21,217). 실로그 검증 기록은
[`fixtures/README.md`](fixtures/README.md) 참고.

</details>

## 🛠️ 개발

```sh
npm run tauri dev                                     # 개발 실행
cargo test --manifest-path src-tauri/Cargo.toml       # 백엔드 테스트 (39개)
npm run check                                         # 타입체크
python3 scripts/capture_fixtures.py --verify-scrub    # 픽스처 민감정보 검사
```

**새 도구 파서 추가** — `src-tauri/src/sources/`에 `UsageSource` trait 구현 파일 1개 +
`registry()` 등록 1줄이면 끝. 코어 집계/UI는 수정 불필요.

**가격표 갱신** — `src-tauri/src/pricing.rs`의 상수 테이블 수정.

## ⚠️ 알려진 제한

- 비용은 API 정가 환산 참고치 — 구독 실지출이 아님
- Claude 한도는 추정치 (플랜 상한을 로컬에서 알 수 없어 잔여 %는 미표시)
- "오늘" 경계는 로컬 타임존 자정 — UTC 기준 도구와 수치가 다를 수 있음
- Windows 빌드는 CI 산출물이며 실기기 검증은 macOS 대비 제한적 (트레이 텍스트 → 툴팁)
