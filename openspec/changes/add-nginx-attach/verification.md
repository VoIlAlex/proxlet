# Verification

Each scenario in `specs/` mapped to the test that exercises it. Paths are
relative to `crates/`.

- `attach::`, `cli::` and `daemon::` are integration tests that run the real
  `proxlet` binary in an isolated `PROXLET_HOME`. `attach::` uses a fake nginx:
  a real config tree plus a script that answers `nginx -V`, `-t` and
  `-s reload`.
- The CI jobs `systemd` and `nginx` run on GitHub's Ubuntu runner with real
  systemd and a real, apt-installed nginx.

On macOS, `cargo test --workspace` passes all 222 tests, and
`cargo clippy -- -D warnings` and `cargo fmt --check` are clean.

## front-attach

| Scenario | Test |
| --- | --- |
| Unclaimed host reaches proxlet | CI `nginx` › "start attaches behind nginx"; `proxlet-front/tests/nginx_layouts.rs` plan tests |
| Claimed host stays with nginx | CI `nginx` › "start attaches behind nginx" (`site.test` answered by nginx); `proxlet-front/src/config.rs` name-matching tests |
| WebSocket through nginx | CI `nginx` › "websocket through nginx"; `proxlet-front/src/nginx/mod.rs` `rendered_block_is_marked_and_points_at_the_internal_listener` |
| nginx rejects the change | `proxlet/src/commands/service.rs` `nginx_rejecting_the_change_leaves_everything_as_it_was`; `proxlet-front/src/apply.rs` `failed_check_restores_every_file`, `failed_reload_restores_every_file`, `failed_write_restores_earlier_ones` |
| Take over with confirmation | `service.rs` `take_over_with_confirmation_then_stop_restores_everything`; `nginx_layouts.rs` `plan_on_debian_moves_both_default_flags_through_the_symlink` |
| Take over without a prompt | `service.rs` `take_over_without_a_prompt`; CI `nginx` (`sudo proxlet start --yes`) |
| Declined | `service.rs` `declined_or_no_terminal_leaves_nginx_config_unchanged`; `attach::run_without_a_terminal_keeps_nginxs_default_server` |
| nginx starts while proxlet holds port 80 | CI `nginx` › "nginx stops and starts as a service"; `proxlet-service/src/systemd.rs` `hook_and_unhook_write_and_remove_the_drop_in`; `daemon::handoff_release_frees_the_port_and_reclaim_takes_it_back` |
| proxlet not running | `daemon::handoff_succeeds_when_proxlet_is_not_running`; `systemd.rs` `drop_in_never_blocks_the_other_unit` (`-` prefix) |
| nginx started by hand | CI `nginx` › "nginx started by hand"; `attach::a_bind_failure_in_nginxs_log_hands_the_port_over`; `proxlet/src/watch.rs` tail tests |
| nginx fails for another reason | `proxlet/src/ports.rs` `release_is_held_while_the_front_server_starts`, `failed_front_start_is_reclaimed_quickly`; `watch.rs` `process_edge_fires_once_per_start` |
| nginx stopped | CI `nginx` › "nginx stops and starts as a service"; `attach::behind_nginx_status_and_reclaim`; `ports.rs` `release_closes_the_listener_and_front_takes_over` |
| Stop while attached | `service.rs` `take_over_with_confirmation_then_stop_restores_everything`; CI `nginx` › "stop restores nginx exactly" (`diff -r`); `apply.rs` `apply_then_undo_restores_byte_for_byte`, `undo_leaves_user_edits_alone`, `undo_finds_a_moved_line` |
| nginx installed later | `attach::behind_nginx_status_and_reclaim` (status hint) |
| Setting up the attach | `service.rs` `setting_up_the_attach_while_already_running` |
| Conflict confirmed | `attach::conflict_confirmed_with_yes_is_marked_in_ls`; `proxlet/src/commands/rules.rs` `conflict_answers`; CI `nginx` › "a host nginx serves needs confirmation" |
| Conflict without a terminal | `attach::conflict_without_a_terminal_adds_nothing`; CI `nginx` |
| Conflict declined | `rules.rs` `conflict_answers` |
| Attach disabled | `service.rs` `attach_disabled_touches_nothing`; `attach::attach_disabled_reads_no_nginx_config`; `ports.rs` `attach_off_treats_nginx_like_anything_else` |

## daemon-lifecycle

| Scenario | Test |
| --- | --- |
| Port free | `daemon::run_serves_rules_and_stops_cleanly_on_sigterm`; `ports.rs` `free_port_is_standalone` |
| Port held by nginx | `attach::behind_nginx_status_and_reclaim`; `ports.rs` `port_held_by_front_server_means_attached` |
| Port taken | `daemon::port_taken_waits_and_names_the_holder`; `ports.rs` `port_held_by_another_process_waits_then_takes_it` |
| Port freed while waiting | `daemon::port_taken_waits_and_names_the_holder`; CI `systemd` › "a taken port means waiting, then taking it" |
| A second proxlet | `daemon::second_run_says_already_running` |
| First start on a systemd host | `service.rs` `first_start_installs_runs_as_invoking_user_and_waits`; CI `systemd` |
| Already running | `service.rs` `start_when_already_running_changes_nothing`; CI `systemd` |
| Start on a taken port | `service.rs` `start_on_a_taken_port_succeeds_and_waits_for_it`, `start_waits_until_the_port_has_been_checked`; CI `systemd` |
| Start fails | `service.rs` `start_failure_shows_the_servers_error_and_stops_retrying`; CI `systemd` › "a failed start is explained" (broken config file) |
| Running standalone | `daemon::status_reports_running_details`; `status.rs` `listen_line_follows_the_mode` |
| Running behind nginx | `attach::behind_nginx_status_and_reclaim`; `status.rs` `listen_line_follows_the_mode` |
| Not running | `daemon::status_when_stopped_exits_nonzero_with_hint` |

## proxy-rules

| Scenario | Test |
| --- | --- |
| List rules | `cli::ls_plain_output_and_empty_case`; `rules.rs` `table_is_aligned`, `plain_is_tab_separated` |
| Rule shadowed by nginx | `attach::conflict_confirmed_with_yes_is_marked_in_ls`; `rules.rs` `table_shows_notes_only_when_there_are_some` |
| Empty list | `cli::ls_plain_output_and_empty_case` |

## settings

| Scenario | Test |
| --- | --- |
| Defaults on a fresh install | `proxlet-core/src/settings.rs` `defaults` |
| Duration value | `cli::proxy_timeout_setting_bounds_upstream_wait`, `cli::config_duration_value` |
| Attach port needs a restart | `cli::attach_port_needs_a_restart` |

## Not yet verified

- The CI jobs `systemd` (changed) and `nginx` (new) have not run yet. They run
  on the first push of this change.
