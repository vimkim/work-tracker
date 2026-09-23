#!/bin/bash
set -euo pipefail

if [[ "${WORK_TRACKER_GITHUB_SMOKE:-}" != "1" ]]; then
    printf '%s\n' \
        'refusing live GitHub smoke: set WORK_TRACKER_GITHUB_SMOKE=1 to opt in' >&2
    exit 2
fi

for command_name in cargo gh python3; do
    if ! command -v "$command_name" >/dev/null 2>&1; then
        printf 'required command is unavailable: %s\n' "$command_name" >&2
        exit 2
    fi
done

gh auth status >/dev/null

smoke_authenticated_owner="$(gh api user --jq .login)"
smoke_owner="${1:-${smoke_authenticated_owner}}"
if [[ "${smoke_owner,,}" == "${smoke_authenticated_owner,,}" ]]; then
    smoke_owner="$smoke_authenticated_owner"
else
    smoke_owner="$(gh api "orgs/${smoke_owner}" --jq .login)"
fi
smoke_started_at="$(date -u +%Y%m%dT%H%M%SZ)"
smoke_nonce="$(python3 -c 'import uuid; print(uuid.uuid4())')"
smoke_nonce_pattern='^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$'
if [[ ! "$smoke_nonce" =~ $smoke_nonce_pattern ]]; then
    printf 'refusing live GitHub smoke with invalid UUID nonce: %s\n' "$smoke_nonce" >&2
    exit 2
fi
smoke_run_id="${smoke_started_at}-${smoke_nonce}"
smoke_name="work-tracker-disposable-smoke-${smoke_run_id}"
smoke_repository="${smoke_owner}/${smoke_name}"
smoke_repository_id=""
smoke_provenance="work-tracker-live-smoke:${smoke_nonce}"
smoke_root="$(mktemp -d)"
smoke_cleanup_candidate=0

smoke_original_config_base="${XDG_CONFIG_HOME:-${HOME}/.config}"
export GH_CONFIG_DIR="${GH_CONFIG_DIR:-${smoke_original_config_base}/gh}"
export XDG_CONFIG_HOME="${smoke_root}/config"
export XDG_DATA_HOME="${smoke_root}/data"
export WORK_TRACKER_ACTOR="github-live-smoke"

cleanup() {
    cleanup_status=$?
    trap - EXIT INT TERM
    if [[ "$smoke_cleanup_candidate" == "1" ]]; then
        cleanup_identity="$(gh api "repos/${smoke_repository}" \
            --jq '[.id, .full_name, .private, .description] | @tsv' 2>/dev/null || true)"
        IFS=$'\t' read -r cleanup_repository_id cleanup_full_name cleanup_private \
            cleanup_provenance <<<"$cleanup_identity"
        cleanup_id_matches=0
        if [[ -z "$smoke_repository_id" || "$cleanup_repository_id" == "$smoke_repository_id" ]]; then
            cleanup_id_matches=1
        fi
        if [[ "$smoke_nonce" =~ $smoke_nonce_pattern ]] &&
            [[ "$smoke_name" == "work-tracker-disposable-smoke-${smoke_started_at}-${smoke_nonce}" ]] &&
            [[ "$cleanup_repository_id" =~ ^[0-9]+$ ]] &&
            [[ "$cleanup_id_matches" == "1" ]] &&
            [[ "$cleanup_full_name" == "$smoke_repository" ]] &&
            [[ "$cleanup_private" == "true" ]] &&
            [[ "$cleanup_provenance" == "$smoke_provenance" ]]; then
            smoke_repository_id="$cleanup_repository_id"
            if ! gh repo delete "$smoke_repository" --yes >/dev/null 2>&1; then
                printf 'warning: failed to delete disposable private repository %s\n' \
                    "$smoke_repository" >&2
                [[ "$cleanup_status" != "0" ]] || cleanup_status=1
            fi
        else
            printf 'warning: refusing cleanup of repository that failed the private disposable guard: %s\n' \
                "$smoke_repository" >&2
            [[ "$cleanup_status" != "0" ]] || cleanup_status=1
        fi
    fi
    if [[ -n "$smoke_root" && -d "$smoke_root" ]]; then
        rm -rf -- "$smoke_root"
    fi
    exit "$cleanup_status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

printf 'building work-tracker for live smoke...\n'
cargo build --quiet
smoke_binary="$(pwd)/target/debug/work-tracker"

printf 'creating disposable private ledger %s...\n' "$smoke_repository"
if [[ "$smoke_owner" == "$smoke_authenticated_owner" ]]; then
    smoke_create_endpoint="user/repos"
else
    smoke_create_endpoint="orgs/${smoke_owner}/repos"
fi
smoke_preflight_error="${smoke_root}/preflight.stderr"
if gh api "repos/${smoke_repository}" --silent 2>"$smoke_preflight_error"; then
    printf 'refusing to reuse an existing repository: %s\n' "$smoke_repository" >&2
    exit 1
elif ! grep -Eq 'Not Found.*HTTP 404|HTTP 404.*Not Found' "$smoke_preflight_error"; then
    printf 'could not establish that the disposable repository name is absent:\n' >&2
    cat "$smoke_preflight_error" >&2
    exit 1
fi
smoke_cleanup_candidate=1
smoke_created_identity="$(gh api --method POST "$smoke_create_endpoint" \
    -f name="$smoke_name" -f description="$smoke_provenance" \
    -F private=true -F has_wiki=false \
    --jq '[.id, .full_name, .private, .description] | @tsv')"
smoke_repository_id="${smoke_created_identity%%$'\t'*}"
if [[ "$smoke_created_identity" != "${smoke_repository_id}"$'\t'"${smoke_repository}"$'\t'true$'\t'"${smoke_provenance}" ]] ||
    [[ ! "$smoke_repository_id" =~ ^[0-9]+$ ]]; then
    printf 'created repository failed identity/private verification: %s\n' \
        "$smoke_repository" >&2
    exit 1
fi
"$smoke_binary" --json init github "$smoke_repository" >/dev/null

smoke_create_id="smoke-create-${smoke_run_id}"
smoke_item_json="$($smoke_binary --repository "$smoke_repository" --json add \
    'Disposable smoke Work Item' \
    --description 'Created only for the opt-in live GitHub smoke.' \
    --status pending \
    --note 'live smoke creation' \
    --event-id "$smoke_create_id")"
smoke_item_id="$(python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])' <<<"$smoke_item_json")"

"$smoke_binary" --repository "$smoke_repository" show "$smoke_item_id" >/dev/null
"$smoke_binary" --repository "$smoke_repository" --json list >/dev/null
"$smoke_binary" --repository "$smoke_repository" --json today >/dev/null
"$smoke_binary" --repository "$smoke_repository" --json update "$smoke_item_id" \
    --description 'Updated by the opt-in live GitHub smoke.' \
    --note 'exercise field mutation' \
    --event-id "smoke-update-${smoke_run_id}" >/dev/null
"$smoke_binary" --repository "$smoke_repository" status "$smoke_item_id" active \
    --note 'exercise Status mutation' \
    --event-id "smoke-status-${smoke_run_id}" >/dev/null
"$smoke_binary" --repository "$smoke_repository" --json note "$smoke_item_id" \
    'exercise standalone History Entry' \
    --event-id "smoke-note-${smoke_run_id}" >/dev/null
"$smoke_binary" --repository "$smoke_repository" --json history "$smoke_item_id" >/dev/null
"$smoke_binary" --repository "$smoke_repository" --json status "$smoke_item_id" active >/dev/null
"$smoke_binary" --repository "$smoke_repository" --json doctor "$smoke_item_id" >/dev/null
"$smoke_binary" --repository "$smoke_repository" --json archive "$smoke_item_id" \
    --note 'live smoke complete' \
    --event-id "smoke-archive-${smoke_run_id}" >/dev/null
"$smoke_binary" --repository "$smoke_repository" --json show "$smoke_item_id" >/dev/null

smoke_cache="$($smoke_binary --repository "$smoke_repository" --json path | \
    python3 -c 'import json,sys; print(json.load(sys.stdin)["cache"])')"
mv -- "$smoke_cache" "${smoke_cache}.before-rebuild"
"$smoke_binary" --repository "$smoke_repository" --fresh --json \
    list --status archived >/dev/null

printf 'live GitHub smoke passed for %s; cleanup will delete it now.\n' "$smoke_repository"
