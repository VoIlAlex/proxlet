# Verification

Each scenario in `specs/` mapped to the test that exercises it. Paths are
relative to `crates/`; `cli::` and `daemon::` are integration tests that run the
real `proxlet` binary in an isolated `PROXLET_HOME`.

`openspec validate add-core-proxy --strict` passes. On macOS, `cargo test
--workspace` passes all 140 tests, and `cargo clippy -- -D warnings` and `cargo
fmt --check` are clean.

## proxy-rules

| Scenario | Test |
| --- | --- |
| Path-only route | `proxlet-core/src/route.rs` `path_only_route` |
| Host route | `route.rs` `host_route_is_lowercased_and_trailing_slash_dropped` |
| Bare host | `route.rs` `bare_host_means_root` |
| Invalid route | `route.rs` `invalid_routes_are_rejected`; `cli::add_rejects_bad_input_and_changes_nothing` |
| Port target | `proxlet-core/src/target.rs` `port_target` |
| Upstream target | `target.rs` `host_port_target`, `url_targets` |
| Directory target | `target.rs` `directory_targets_resolve_against_cwd`, `validate_directory` |
| Directory whose name is a number | `target.rs` `numeric_directory_name_with_dot_slash`; `cli::numeric_directory_with_dot_slash_is_a_directory` |
| Missing directory | `target.rs` `validate_missing_directory`; `cli::add_rejects_bad_input_and_changes_nothing` |
| Upstream not listening yet | `cli::add_warns_when_nothing_listens` |
| Add while running | `cli::add_while_running_applies_immediately` |
| Add while stopped | `cli::add_while_stopped_saves_and_says_so` |
| Persistence across restart | `cli::rule_survives_restart` |
| Update target | `cli::add_existing_route_updates_it`; `proxlet-core/src/config.rs` `add_update_remove_round_trip` |
| Remove existing rule | `cli::rm_existing_and_unknown` |
| Remove unknown rule | `cli::rm_existing_and_unknown`; `config.rs` `remove_unknown_rule_fails_and_leaves_file_unchanged` |
| List rules | `cli::ls_plain_output_and_empty_case`; `proxlet/src/commands/rules.rs` `table_is_aligned`, `plain_is_tab_separated` |
| Empty list | `cli::ls_plain_output_and_empty_case` |
| Complete rm | `cli::completion_offers_rules_keys_and_values` |

## http-serving

| Scenario | Test |
| --- | --- |
| Longest path wins | `proxlet-core/src/table.rs` `longest_path_wins`, property test `agrees_with_naive_matcher` |
| Host rule beats hostless rule | `table.rs` `host_rule_beats_hostless_rule` |
| Fallback to hostless rule | `table.rs` `fallback_to_hostless_rule` |
| Segment boundary | `table.rs` `segment_boundary`; `route.rs` `segment_boundary_matching` |
| Unmatched request | `proxlet-server/tests/proxy.rs` `unmatched_request_gets_404_from_proxlet` |
| Path is preserved | `proxy.rs` `path_and_query_are_preserved` |
| Streaming response | `proxy.rs` `streaming_response_is_not_buffered` |
| Forwarded headers present | `proxy.rs` `forwarding_headers_and_hop_by_hop`; `proxlet-server/src/proxy.rs` `forwarded_for_appends` |
| WebSocket through proxlet | `proxy.rs` `websocket_through_proxlet` |
| Upstream down | `proxy.rs` `upstream_down_is_502_naming_route_and_upstream` |
| Upstream too slow | `proxy.rs` `stalled_upstream_is_504` |
| File under a prefix | `proxlet-server/tests/files.rs` `file_under_prefix_with_content_type` |
| Missing file | `files.rs` `missing_file_is_404` |
| Range request | `files.rs` `range_requests` |
| Index file | `files.rs` `index_file_is_served_for_directories` |
| Listing on | `files.rs` `listing_on_by_default` |
| Listing off | `files.rs` `listing_off_is_404` |
| Directory without a trailing slash | `files.rs` `directory_without_slash_redirects` |
| Path traversal | `files.rs` `path_traversal_never_leaves_the_directory`, `symlink_out_of_the_directory_is_refused` |
| Dotfile hidden by default | `files.rs` `dotfiles_hidden_by_default_and_served_when_enabled` |

## daemon-lifecycle

| Scenario | Test |
| --- | --- |
| Run and interrupt | `daemon::ctrl_c_shuts_down_cleanly`, `daemon::run_serves_rules_and_stops_cleanly_on_sigterm`; `proxlet-server/tests/shutdown.rs` |
| Port free | `daemon::run_serves_rules_and_stops_cleanly_on_sigterm` |
| Port taken | `daemon::port_taken_names_port_and_holder` |
| A second proxlet | `daemon::second_run_says_already_running` |
| First start on a systemd host | `proxlet/src/commands/service.rs` `first_start_installs_runs_as_invoking_user_and_waits` (fake manager); `proxlet-service/src/systemd.rs` unit-file and command tests; CI job `systemd` (real systemd) |
| Already running | `service.rs` `start_when_already_running_changes_nothing`; CI job `systemd` |
| Start fails | `service.rs` `start_failure_shows_the_servers_error_and_stops_retrying`; CI job `systemd` |
| Start on macOS | `service.rs` `start_without_a_service_manager_points_to_run` |
| Stop on macOS with nothing running | `service.rs` `stop_with_nothing_running_and_no_manager_fails` |
| Start without root | `service.rs` `start_without_root_asks_for_sudo` |
| Stop a running service | `service.rs` `stop_running_service`; CI job `systemd` |
| Already stopped | `service.rs` `stop_when_not_running_succeeds`; CI job `systemd` |
| Stop a foreground server | `service.rs` `stop_foreground_daemon_without_service_manager`; `daemon::shutdown_request_stops_the_daemon` |
| Running standalone | `daemon::status_reports_running_details` |
| Not running | `daemon::status_when_stopped_exits_nonzero_with_hint` |

## settings

| Scenario | Test |
| --- | --- |
| List settings | `cli::config_list_read_and_defaults` |
| Read one setting | `cli::config_list_read_and_defaults` |
| Unknown key | `cli::config_unknown_key_and_invalid_value`; `proxlet-core/src/settings.rs` `unknown_key_suggests_closest` |
| Change applies live | `cli::config_change_applies_live`; `files.rs` `listing_setting_applies_live` |
| Invalid value | `cli::config_unknown_key_and_invalid_value` |
| Restart needed | `cli::config_restart_needed_notice` |
| Defaults on a fresh install | `settings.rs` `defaults` |
| Duration value | `cli::proxy_timeout_setting_bounds_upstream_wait`, `cli::config_duration_value` |
| Complete a key | `cli::completion_offers_rules_keys_and_values` |
| Complete a value | `cli::completion_offers_rules_keys_and_values`; `proxlet/src/complete.rs` tests |

## Real systemd

The CI job `systemd` passed on PR #3 (run 45151497596) on GitHub's Ubuntu
runner. It covers `sudo proxlet start`, the service user, live `add`, serving,
`stop`, a repeated `start` and `stop`, and a start on a taken port 80.
