# 조직 리포팅 (Org Reporting)

meterly를 회사에서 사용량 수집용으로 쓸 때의 에이전트↔수집서버 계약.
**설정이 없으면 이 기능은 완전히 꺼져 있고, 개인용 동작은 그대로다.**

## 동작 개요

```
[meterly (직원 PC)] --(1) POST /register (최초 1회)--> [수집 서버]
[meterly (직원 PC)] --(2) POST /usage    (1시간마다)--> [수집 서버] <-- 조회 -- [별도 어드민]
```

- **신원** = 사용자가 입력한 식별자(예: 사번) + 호스트명. 같은 식별자를 여러
  사람이 쓰는 경우 호스트명으로 구분 가능하며, 등록 기록으로 서버가 감사할 수 있다.
- **전송 데이터** = 일별×도구×모델 토큰 합계뿐. 프롬프트/코드/프로젝트명/계정
  이메일은 전송하지 않는다.
- **스냅샷 업서트**: 매 전송마다 보관 기간(대시보드 범위, 수 개월) 전체의 일별
  행을 다시 보낸다. 서버는 `(user_id, hostname, date, source, model)` 키로
  업서트하면 되고, 미전송 기간이 있어도 다음 전송에서 자동 복원된다.

## 에이전트 설정

우선순위: **관리 파일 > 설정 화면**. 식별자는 항상 개인이 설정 화면에 입력.

1. **관리 파일 (IT 배포)** — url/token을 기기 전체에 강제:
   - macOS: `/Library/Application Support/meterly/managed.json`
   - 그 외: `<데이터 디렉터리>/com.meterly.app/managed.json`
   ```json
   { "url": "https://collect.example.com", "token": "org-shared-secret" }
   ```
2. **설정 화면** — 사용자가 URL·토큰(선택)·식별자를 입력하고 [등록].

등록(2xx) 후부터 1시간 간격으로 전송한다. 실패 시 다음 새로고침 주기에 재시도.
모든 요청에 `Authorization: Bearer <token>`(토큰 설정 시)과
`User-Agent: meterly/<버전>`이 붙는다.

## 엔드포인트 계약 (서버가 구현할 것)

### POST {url}/register — 최초 등록
```json
{ "schema": 1, "user_id": "E12345", "hostname": "Jays-MacBook-Pro", "app_version": "0.1.16" }
```
- 2xx = 등록 성공(본문 무관). 그 외 상태코드+본문은 설정 화면에 오류로 표시된다.
- 서버는 (user_id, hostname, 최초 등록 시각)을 기록한다. 이미 있는 조합의
  재등록은 idempotent하게 2xx를 권장.

### POST {url}/usage — 사용량 스냅샷 (1시간마다)
```json
{
  "schema": 1,
  "user_id": "E12345",
  "hostname": "Jays-MacBook-Pro",
  "app_version": "0.1.16",
  "reported_at": "2026-07-16T13:02:10Z",
  "daily": [
    { "date": "2026-07-16", "source": "claude_code", "model": "claude-sonnet-5",
      "input": 6771519, "output": 491702, "cache_read": 143323136, "cache_creation": 0 }
  ]
}
```
- `source`: `claude_code` | `codex` (도구가 늘면 값 추가 — 스키마 변경 없음)
- `model`: 원문 모델 ID, 없으면 `null`
- 토큰 4종은 서로 배타 합산 가능(`total = input+output+cache_read+cache_creation`)
- 2xx = 수신 성공. 그 외엔 클라이언트가 재시도한다(스냅샷이라 중복 안전).

## 어드민에서 (별도 시스템, 참고)

- **개인별 통계/검색**: `(user_id, date, source, model)` 테이블로 충분.
- **미설치자 식별**: `/register` 기록(또는 `/usage`의 user_id 집합)과 조직
  명부(HR/Claude 팀 멤버 목록)를 대조. `reported_at`이 오래된 사용자는
  "설치했지만 미사용/중지"로 구분 가능.
- 비용 환산이 필요하면 모델×토큰 단가표를 어드민 쪽에 두는 것을 권장
  (에이전트 단가와 버전 차이가 생기지 않도록 서버 기준 단일화).

## 스키마 변경 정책

`schema` 필드가 계약 버전이다. 하위 호환이 깨지는 변경 시에만 +1 하고,
서버는 모르는 필드를 무시해야 한다(에이전트가 필드를 추가할 수 있음).
