# GitHub ledger release qualification

This is the acceptance-to-evidence index for parent issue #1 and final qualification issue #15.
All automated evidence runs with an isolated home, configuration directory, cache, and scripted
fake `gh`; `tests/release_qualification.rs` proves that the separately documented live procedure
cannot invoke `gh` without explicit opt-in.

## Parent #1 acceptance statements

| ID | Acceptance | Deterministic evidence |
|---|---|---|
| P-01 | Initialization is explicit. | `github_init::init_github_creates_a_private_default_ledger_and_cache` |
| P-02 | The default repository is private. | `github_init::init_github_creates_a_private_default_ledger_and_cache` asserts `private=true`. |
| P-03 | Initialization is safely repeatable. | `github_init::repeating_init_is_idempotent` |
| P-04 | Initialization validates visibility, Issues, permission, and compatibility. | `github_init::{github_failure_categories_have_distinct_json_diagnostics,human_diagnostics_distinguish_visibility_permission_and_compatibility,existing_issue_must_have_exactly_one_status_label}` |
| P-05 | One configured repository is the default. | `github_init::init_github_creates_a_private_default_ledger_and_cache` |
| P-06 | Commands support a repository override. | `github_init::explicit_repository_override_does_not_rewrite_the_default` |
| P-07 | Repository and cache location are discoverable. | `cli_compatibility::default_path_and_storage_are_isolated` and the live smoke's JSON `path` query. |
| P-08 | The CLI creates a Work Item. | `github_add_show::add_creates_a_github_work_item_and_show_reads_the_synchronized_cache` |
| P-09 | Successful mutations preserve stable JSON and exit zero. | Compiled-CLI assertions throughout `github_{add_show,update,status,note_history,archive}`. |
| P-10 | Validation, conflict, integrity, auth, and availability errors are nonzero and structured. | `github_init` failure-category tests, `cache_freshness::{fresh_read_fails_instead_of_using_cached_data,cache_is_unavailable_until_one_sync_has_succeeded}`, `github_update::first_valid_proposal_wins_and_rejected_mutation_gets_current_state`, and `github_note_history::edited_history_raises_ledger_integrity_error_before_note_mutation_or_repair`. |
| P-11 | GitHub title and body are readable projections. | `github_add_show::add_creates_a_github_work_item_and_show_reads_the_synchronized_cache` |
| P-12 | GitHub issue number is the Work Item ID. | `github_add_show::add_creates_a_github_work_item_and_show_reads_the_synchronized_cache` |
| P-13 | Work Items with an actionable Status remain open issues. | `github_status::actionable_status_transition_advances_revision_and_replaces_projection_label` and `github_sync::github_lists_preserve_filters_limits_attention_order_and_viewer_local_day` |
| P-14 | Done closes as completed. | `github_status::done_closes_as_completed_and_can_reopen_to_an_actionable_status` |
| P-15 | Cancelled and archived close not-planned with distinct labels. | `github_status::cancelled_closes_as_not_planned_and_can_reopen` and `github_archive::archive_records_one_event_closes_not_planned_and_locks` |
| P-16 | Every managed issue has exactly one Status label. | `github_init::existing_issue_must_have_exactly_one_status_label` and Status projection tests. |
| P-17 | Done and cancelled remain mutable. | `github_status::{done_closes_as_completed_and_can_reopen_to_an_actionable_status,cancelled_closes_as_not_planned_and_can_reopen}` |
| P-18 | Archived Work Items are immutable, closed, and locked. | `github_archive::archive_records_one_event_closes_not_planned_and_locks` and `cli_compatibility::archived_work_items_are_readable_but_immutable` |
| P-19 | `delete` and `deleted` remain accepted aliases. | `cli_compatibility::deprecated_deletion_inputs_select_archival_and_emit_only_canonical_language` |
| P-20 | Output uses archive terminology. | `cli_compatibility::{archive_emits_canonical_status_and_additive_compatibility_metadata,deprecated_deletion_inputs_select_archival_and_emit_only_canonical_language}` |
| P-21 | Standalone notes become typed History Entries. | `github_note_history::note_publishes_one_canonical_event_and_returns_trusted_attribution` |
| P-22 | Accepted history records kind, Actor, GitHub account, time, note, and changes. | `github_update::accepted_field_update_advances_revision_once_and_records_exact_changes` and `github_note_history::note_publishes_one_canonical_event_and_returns_trusted_attribution` |
| P-23 | Stable event IDs make retries idempotent. | `github_{update,status,archive}` stable-event recovery tests and `github_note_history::retrying_a_known_event_id_returns_the_prior_entry_without_another_comment` |
| P-24 | Creation resumes without duplicate Work Items or history. | `github_add_show::{retry_after_genesis_publication_failure_completes_the_existing_issue,retry_after_projection_failure_reuses_the_published_genesis_event,retry_after_lost_issue_response_ignores_foreign_issues_and_finds_the_pending_marker}` |
| P-25 | Synchronization observes remote events before mutation. | Online-preflight tests in `github_update`, plus replay-before-submit conflict tests. |
| P-26 | First valid field or Status mutation wins by comment order. | `github_update::first_valid_proposal_wins_and_rejected_mutation_gets_current_state` and `github_status::first_valid_status_wins_and_loser_gets_the_human_conflict` |
| P-27 | Stale mutations return revisions and refreshed values. | `github_update::first_valid_proposal_wins_and_rejected_mutation_gets_current_state` |
| P-28 | Stale proposals are retained but excluded from effective history. | The same field/Status conflict tests query both `history` and `rejected`. |
| P-29 | Concurrent notes are both accepted in comment order. | `github_note_history::concurrent_notes_are_both_accepted_in_comment_order_without_advancing_state_revision` |
| P-30 | Repeating a Status is idempotent. | `github_status::repeated_status_is_idempotent_and_publishes_no_proposal` |
| P-31 | Projection drift is detected and repaired. | `github_update::stable_event_id_recovers_an_update_whose_publication_response_was_lost` and projection-repair synchronization tests. |
| P-32 | Human comments are preserved and ignored as ledger history. | `github_note_history::history_replays_github_comment_order_ignores_discussion_and_populates_the_cache` |
| P-33 | Edited, deleted, disconnected, and reordered evidence is detected. | `github_integrity` edited/deleted/disconnected/hash-continuity qualification tests. |
| P-34 | Integrity-broken Work Items remain inspectable. | `github_integrity::edited_event_stays_inspectable_and_doctor_reports_exact_recovery_evidence` |
| P-35 | Integrity-broken Work Items reject mutation. | `github_note_history::edited_history_raises_ledger_integrity_error_before_note_mutation_or_repair` |
| P-36 | Doctor diagnoses without mutation. | `github_integrity::doctor_is_read_only_and_does_not_misclassify_projection_drift` |
| P-37 | Exact restore requires a verified copy. | `github_integrity::{exact_recovery_restores_verified_copy_revalidates_and_rebuilds_projection,doctor_never_offers_exact_recovery_for_unverified_cached_evidence}` |
| P-38 | Rebaseline is explicit, attributed, and retains damaged evidence. | `github_integrity::explicit_rebaseline_retains_untrusted_evidence_and_starts_a_new_hash_root` |
| P-39 | Integrity repair is never automatic. | `github_sync::unknown_headless_projection_is_inspectable_but_not_automatically_rebaselined` and doctor/recovery CLI parsing tests. |
| P-40 | Normal reads synchronize first. | `cache_freshness::every_normal_read_attempts_sync_and_human_output_warns_when_stale` |
| P-41 | Outages fall back with a prominent stale warning. | `cache_freshness::json_show_falls_back_to_cache_with_a_structured_stale_warning` and its human-output companion. |
| P-42 | JSON data stays on stdout and stale warning on stderr. | `cache_freshness::json_show_falls_back_to_cache_with_a_structured_stale_warning` |
| P-43 | `--fresh` forbids stale fallback. | `cache_freshness::fresh_read_fails_instead_of_using_cached_data` |
| P-44 | `--offline` skips the network. | `cache_freshness::offline_read_uses_cache_without_invoking_github` |
| P-45 | Offline writes fail before local mutation. | `cache_freshness::offline_write_fails_before_invoking_github` |
| P-46 | Dashboard exposes synchronization and stale state. | `web::tests::both_dashboard_routes_render_fresh_stale_unavailable_and_integrity_states` |
| P-47 | Dashboard Daily View and item history remain read-only. | `web::tests::{dashboard_routes_read_through_the_ledger,dashboard_rejects_mutating_http_methods}` |
| P-48 | Daily View uses the viewer's local day and includes every Actionable Work Item. | `github_sync::github_lists_preserve_filters_limits_attention_order_and_viewer_local_day` fixes the subprocess clock and timezone, covers exact local-day boundaries, and includes pending, active, waiting, and blocked fixtures. |
| P-49 | List filters and attention order are preserved. | `github_sync::github_lists_preserve_filters_limits_attention_order_and_viewer_local_day` and database ordering tests. |
| P-50 | The disposable cache rebuilds entirely from GitHub. | Cacheless creation, rejected-mutation, archived, integrity, and freshness rebuild tests indexed in the #15 matrix below. |
| P-51 | Existing SQLite data is not uploaded. | `github_init::configured_github_default_never_silently_writes_the_legacy_sqlite_ledger` |
| P-52 | Existing SQLite data remains available through an explicit backend. | `cli_compatibility::compiled_cli_preserves_the_sqlite_lifecycle_contract` and README legacy commands. |
| P-53 | Legacy deleted data migrates to archival without purge. | `cli_compatibility::{opening_a_legacy_database_archives_deleted_items_without_purging_history,concurrent_legacy_opens_apply_the_migration_once}` |
| P-54 | Authentication is delegated to `gh`. | `github_init::missing_gh_has_a_distinct_machine_readable_diagnostic`; README authentication runbook. |
| P-55 | GitHub transport is behind a narrow adapter. | `ledger_seam::sqlite_lifecycle_is_available_through_the_ledger_seam`; `src/github.rs` owns the `gh api` adapter. |
| P-56 | Event schemas are versioned and unknown versions are explicit. | Canonical vector unit test plus `github_note_history::unknown_event_schema_warns_and_never_replaces_trusted_cached_history`. |
| P-57 | Partial failures retain recoverable events and converge. | Projection-failure tests in `github_{add_show,update,status,archive}` and `github_sync::interrupted_cache_commit_rolls_back_items_history_and_cursor_then_retries`. |
| P-58 | GitHub failure classes are distinct. | `github_init::{github_failure_categories_have_distinct_json_diagnostics,github_transport_failures_keep_actionable_json_categories}` |
| P-59 | Archive migration retains old JSON fields additively. | `cli_compatibility::archive_emits_canonical_status_and_additive_compatibility_metadata` |
| P-60 | Ordinary tests never contact real GitHub. | Every GitHub integration test uses `tests/support::FakeGh` with cleared environment; live smoke has a tested explicit opt-in guard. |

## Ticket #15 release criteria

| ID | Criterion | Qualification evidence |
|---|---|---|
| Q-01 | Complete compiled-CLI matrix. | Focused compiled subprocess suites cover initialization, backend selection, lifecycle commands, aliases, override, freshness, doctor, restore, and Rebaseline; `github_integrity::exact_recovery_has_a_human_success_contract` closes the recovery-mode matrix and the public command surface is guarded by `release_qualification::compiled_cli_command_and_exit_contract_is_stable`. |
| Q-02 | Human, JSON, stderr, and exit contracts. | `release_qualification::compiled_cli_command_and_exit_contract_is_stable`, GitHub failure-category tests, stale-output tests, the JSON field-conflict and human Status-conflict tests, human/JSON integrity tests, and human/JSON recovery tests. |
| Q-03 | All concurrency permutations. | Field conflict, Status conflict, repeated Status, concurrent-note, note/Status, and accepted-mutation retry-race tests in `github_{update,status,note_history}`. |
| Q-04 | Failure injection converges without duplicates. | Creation/event/projection/lock interruption tests across `github_{add_show,update,status,sync,archive}`, including the close-bearing PATCH in `github_status::accepted_done_survives_close_projection_failure_and_sync_repairs_it` and `github_update::stable_event_retry_recovers_after_confirmation_read_failure`; `db::tests::cache_batch_failure_at_each_step_rolls_back_and_retry_converges` injects each removal/item/history/rejection/evidence/pending-cleanup step plus both freshness INSERT and UPDATE, compares the complete cache database before failure, and compares the retry with an independently built expected database. |
| Q-05 | Full cache rebuild fidelity. | `github_sync::deleting_the_cache_rebuilds_state_history_rejections_archive_integrity_and_freshness` compares exact pre/post JSON state, accepted hashes/revisions/attribution/changes, Rejected Mutations, integrity report, retained evidence, cursor, and observable freshness time in one compiled-CLI scenario. |
| Q-06 | Both dashboard routes cover health, archive, escaping, and read-only behavior. | All three `web::tests::dashboard_*` router tests. |
| Q-07 | User/operator documentation is complete. | `release_qualification::readme_covers_the_github_user_and_operator_runbook` and README runbooks. |
| Q-08 | Live smoke is opt-in, private, and disposable. | `release_qualification::{live_github_smoke_refuses_to_run_without_explicit_opt_in,live_github_smoke_enforces_nonce_identity_privacy_and_cleanup}` exercise the guard, exact absence preflight and private-create request, timestamp plus v4 UUID nonce, provenance/identity/privacy deletion checks, ambiguous creation response, exact delete target, deletion failure, and INT/TERM cleanup exits; `scripts/github-live-smoke.sh` is absent from `just check`. |
| Q-09 | Aggregate gate and independent reviews pass. | `just check` plus fresh Standards and Spec reviews are required at the qualifying commit before integration. |

## Ordinary and live boundaries

`just check` is the release gate for deterministic coverage. It never runs `smoke-github`.
The live procedure is an additional operator-triggered confidence check; it is not evidence for a
behavior that can be proved safely with `FakeGh`, and it must never target a durable repository.
