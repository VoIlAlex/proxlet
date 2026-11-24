# Verification

Each scenario in `specs/` mapped to the test that exercises it. Paths are
relative to `crates/`.

- `logs::` and `cli::` are integration tests that run the real `proxlet`
  binary in an isolated `PROXLET_HOME`, read the log files it writes, and run
  `proxlet logs` against them, including following while the server rotates
  (`PROXLET_TEST_LOG_FILE_BYTES` lowers the 10 MB file size).
- `proxlet-server/tests/access.rs` drives the engine with real HTTP and
  WebSocket clients and a recording access-log sink.
- The CI job `systemd` checks `proxlet logs` against the real service on
  GitHub's Ubuntu runner: following, `--all`, `--json`, file ownership and
  mode, and reading after the service stopped.

On macOS, `cargo test --workspace` passes, and `cargo clippy --workspace
--all-targets -- -D warnings` and `cargo fmt --check` are clean.

## logs

| Scenario | Test |
| --- | --- |
| Proxied request | `proxlet-server/tests/access.rs` `proxied_request_without_query`; `logs::requests_and_events_reach_the_log_files`; `proxlet/src/logs/record.rs` `request_json` |
| Upstream down | `access.rs` `upstream_down_records_the_reason`; `record.rs` `error_request_json` |
| Streaming response | `access.rs` `streaming_response_is_one_record_with_totals`, `client_hanging_up_mid_stream_is_recorded`, `websocket_upgrade_is_one_record` |
| Behind nginx | `access.rs` `behind_nginx_the_client_is_the_forwarded_one`; `proxlet-server/src/access.rs` `client_is_peer_or_first_forwarded_entry` |
| Access logging off | `access.rs` `access_logging_off_applies_live`; `logs::access_logging_off_keeps_events` |
| Port taken | `logs::port_taken_is_an_error_event`; `proxlet/src/logs/layer.rs` `records_proxlet_messages_at_info_and_above` |
| Rotation and size limit | `proxlet/src/logs/writer.rs` `rotation_keeps_records_whole_and_within_the_size_limit`; `logs/files.rs` `size_limit_deletes_oldest_and_never_the_current_file`, `private_directory_and_file`; `logs::rotation_while_following` |
| Age limit | `logs/files.rs` `age_limit` (retention runs at start, after each rotation and hourly: `writer.rs` `Writer::run`, `rotate`, `tick`) |
| Unwritable log directory | `logs::unwritable_log_directory_still_serves_and_warns`; `writer.rs` `unwritable_directory_never_blocks_senders`, `full_queue_drops_and_records_how_many` |
| Follow | `logs::follow_shows_the_last_records_then_new_ones`, `follow_before_any_record_and_with_a_selector`; CI `systemd` › "logs from the service" |
| All history | `logs::all_history_selectors_and_reading_while_stopped`, `invalid_selector_and_no_records`; `logs/read.rs` `history_spans_files` |
| Rotation while following | `logs::rotation_while_following`; `logs/read.rs` `follows_across_rotation_without_loss_or_repeat`, `several_rotations_between_polls`, `rotation_before_the_new_file_exists` |
| By route | `logs::all_history_selectors_and_reading_while_stopped`; `logs/show.rs` `selectors`, `matching` |
| By domain | `logs::all_history_selectors_and_reading_while_stopped`; `show.rs` `matching` |
| By upstream port | `logs::all_history_selectors_and_reading_while_stopped`; `show.rs` `matching` |
| Piped | `logs::piped_and_json_output`; `show.rs` `lines_are_aligned`, `status_is_coloured_in_a_terminal`, `date_shown_when_not_today_and_local_offset_applied` |
| JSON | `logs::piped_and_json_output` (byte for byte); CI `systemd` (`python3 -m json.tool --json-lines`) |
| Complete a selector | `logs::complete_a_selector`; `proxlet/src/complete.rs` `log_selectors_offer_routes_domains_and_ports` |

## settings

| Scenario | Test |
| --- | --- |
| Defaults on a fresh install | `proxlet-core/src/settings.rs` `defaults`; `cli::config_list_read_and_defaults` |
| Attach port needs a restart | `cli::attach_port_needs_a_restart` |
| HTTPS port needs a restart | `cli::https_port_needs_a_restart` |
| Redirect applies live | `https::terms_then_issue_then_https_and_redirect` |
| Log limits apply live | `logs/writer.rs` `limits_apply_live_at_the_next_rotation` (the daemon reads the limits from the engine's snapshot at each retention run: `daemon.rs` `start_log_writer`) |
| Duration value | `cli::config_duration_value`; `settings.rs` `duration_values` |
| Days and sizes | `cli::days_and_sizes`; `settings.rs` `duration_values`, `size_values` |
| Size too small | `cli::log_size_too_small`; `settings.rs` `log_size_has_a_minimum` |
| Same port twice | `cli::ports_must_differ_and_nothing_changes`; `settings.rs` `ports_must_differ` |
| Invalid ACME server | `cli::acme_server_values` |
