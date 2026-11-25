# Feature map

Each row is a feature as a user names it, how the user reaches it, where the
code is, and tests that check it. Use these names in issues, pull requests,
and reviews.

| Feature | How a user reaches it | Code | Checked by |
|---|---|---|---|
| AI review with model choice | `argus review <job> --preset <name> --lens <lens>` | `crates/argus-core/src/event.rs` (`ReviewSpec::panel`), `crates/argus-core/src/adapters/llm.rs`, `crates/argus-core/src/views/review.rs` | `a_review_request_asks_each_model_once_about_the_head`, `review_findings_are_parsed_and_anchored_on_the_reviewed_tree` |
| Review packet | `argus packet <job>`, the review screen | `crates/argus-core/src/views/packet.rs` | `the_first_round_puts_agreed_findings_first_and_explains_hunks_from_the_transcript`, `a_lone_high_finding_is_suspicious_and_two_agreeing_ones_block` |
| Verdict, feedback, and merge | `argus decide`, `argus feedback`, `argus merge` | `crates/argus-core/src/views/review.rs`, `crates/argus-core/src/policy.rs`, `crates/argus-host/src/git.rs`, `specs/tla/ReviewGate.tla` | `request_changes_sends_exactly_the_drafts_it_previewed`, `a_merge_needs_an_approval_that_is_still_current`, `a_verdict_on_the_head_then_a_merge_moves_the_target` |
| Formal verification | `argus check <job> tla|lean|device ...` | `crates/argus-core/src/adapters/tlc.rs`, `crates/argus-core/src/adapters/lean.rs`, `crates/argus-core/src/views/checks.rs` | `tlc_output_and_exit_code_must_agree`, `a_clean_lake_build_is_proved`, `a_check_request_runs_once_on_the_head_and_shows_on_the_review_screen` |
| Timeline | The timeline screen, `timeline` op | `crates/argus-core/src/views/timeline.rs`, `crates/argus-core/src/period.rs` | `the_timeline_lists_what_happened_and_its_quarters`, `snapshots_check_errors_and_run_titles_are_items` |
| Progress at a glance | `argus status`, the phone's home screen | `crates/argus-core/src/views/glance.rs`, `crates/argus-core/src/views/progress.rs` | `the_overview_queue_puts_the_most_urgent_first_across_jobs`, `failing_checks_on_the_head_need_attention` |
| Raw logs | `argus raw <job> --fact <id>` | `crates/argus-log/src/`, `crates/argus-core/src/ledger.rs`, `crates/argus-core/src/source.rs` | `the_raw_log_pages_a_source_and_shows_an_authored_event_as_stored`, `crash_at_any_byte_keeps_every_whole_frame` |
| Agent transcripts | Agent runs; `argus import` | `crates/argus-core/src/adapters/claude.rs`, `crates/argus-core/src/adapters/codex.rs`, `crates/argus-host/src/tail.rs` | `a_run_s_transcripts_are_followed_including_ones_that_appear_later`, `an_import_streams_the_file_as_one_source` |
| Stats and insights | `argus stats`, `argus insights`, `argus skill accept` | `crates/argus-core/src/views/stats.rs`, `crates/argus-core/src/views/insights.rs`, `crates/argus-core/src/store.rs` | `stats_count_per_run_and_hour_and_never_double_count_usage`, `stats_count_what_happened_in_a_period_by_group` |
| Live views | Desktop and phone subscriptions | `crates/argus-host/src/pulse.rs`, `crates/argus-api/src/ops.rs`, `ui/packages/argus-client/` | `a_subscription_pushes_its_value_now_and_again_only_when_it_changes` |
| Crash recovery | Restarting the host | `crates/argus-host/src/ledger_thread.rs`, `crates/argus-core/src/views/runs.rs`, `crates/argus-host/src/reaper.rs` | `after_a_crash_interrupted_runs_are_abandoned_and_safe_ones_retried`, `a_host_crash_abandons_the_attempt_and_retries_only_what_is_safe`, `a_run_is_stopped_when_the_host_is_gone` |
| Command line | `argus ops`, `argus call <op> [<input JSON>]`, `argus watch <op>`, `argus hook`, `argus mcp` (MCP tools for agents) | `crates/argus-cli/src/`, `crates/argus-host/src/transport.rs` | `a_call_prints_the_hosts_reply`, `an_agent_says_which_run_it_is_with_its_token`, `a_watch_prints_each_value_the_host_pushes`, `mcp_tools_reach_the_host_as_the_runs_agent` |
