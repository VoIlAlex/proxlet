# Verification

Each scenario in `specs/` mapped to the test that exercises it. Paths are
relative to `crates/`.

- `cert::`, `https::`, `attach::`, `cli::` and `daemon::` are integration tests
  that run the real `proxlet` binary in an isolated `PROXLET_HOME`. They issue
  from a local fake CA (`PROXLET_TEST_FAKE_ACME`; no test ever contacts a real
  ACME server) and resolve names through `PROXLET_TEST_RESOLVE`.
- `proxlet-server/tests/tls.rs` drives the engine's three listeners with real
  TLS clients (rustls, hyper HTTP/1.1 and HTTP/2, tungstenite).
- The CI jobs `acme` and `nginx` run on GitHub's Ubuntu runner against Pebble,
  Let's Encrypt's test ACME server, with real HTTP-01 validation; `nginx` uses
  a real, apt-installed nginx and systemd.

On macOS, `cargo test --workspace` passes, and `cargo clippy --workspace
--all-targets -- -D warnings` and `cargo fmt --check` are clean.

## certificates

| Scenario | Test |
| --- | --- |
| Certificate issued | `cert::certificate_issued_listed_and_served`; CI `acme` › "issue, serve, redirect" |
| Several domains | `cert::several_domains_and_one_fails` |
| Already has one | `cert::already_has_one` |
| Server not running | `cert::server_not_running` |
| Local name | `cert::local_names_are_refused_without_contacting_the_ca`; `proxlet-cert/src/name.rs` `reserved_names` |
| Name does not resolve | `cert::several_domains_and_one_fails` |
| Name served by nginx | `proxlet/src/commands/cert.rs` `check_front` (shares `FrontConfig::claims` with `add`'s tested conflict check); CI `nginx` |
| Name answered by another server | `cert::name_answered_by_another_server` |
| Probe cannot be sent | `cert::probe_cannot_be_sent_warns_and_continues` |
| First issuance | `https::terms_then_issue_then_https_and_redirect`; `proxlet-cert/src/acme.rs` `terms_must_be_accepted_once`, `pebble_issues_a_certificate` (CI `acme`) |
| No terminal | `cert::terms_without_a_terminal` |
| Staging | `cert::staging_is_noted`; `proxlet-core/src/settings.rs` `acme_server_values` |
| Challenge answered behind nginx | `proxlet-server/tests/tls.rs` `challenge_answered_on_every_listener_before_rules` (front listener); CI `nginx` › "https through nginx" (Pebble validates through nginx) |
| Unknown token | `tls::unknown_token_falls_through_to_the_rules` |
| Key permissions standalone | `proxlet-cert/src/store.rs` `put_get_list_remove` (`0600` file, `0700` directory) |
| List | `cert::certificate_issued_listed_and_served`; `cert.rs` `table_has_a_header_and_aligned_days` |
| None | `cert::none_yet` |
| Remove | `cert::remove_and_renew`; `https::renew_now_and_certs_changed`; CI `acme` › "renew, list, remove" |
| Due for renewal | `https::due_certificates_are_renewed_and_served`; `proxlet-cert/src/renew.rs` `a_90_day_certificate_is_due_with_30_days_left`, `a_45_day_certificate_is_due_with_15_days_left` |
| Renewal fails | `https::a_failing_renewal_keeps_the_old_certificate_and_is_reported`; `renew.rs` `failures_back_off_up_to_six_hours`; `acme.rs` `a_failed_challenge_names_the_domain_and_keeps_the_old_certificate` |
| Renew now | `cert::remove_and_renew`; `https::renew_now_and_certs_changed`; CI `acme` |
| Complete a domain to remove | `cert::completion_offers_rule_hosts_and_stored_domains` |

## https-serving

| Scenario | Test |
| --- | --- |
| Request over HTTPS | `tls::https_picks_the_certificate_and_forwards_https`; CI `acme` |
| HTTP/2 | `tls::http2_is_negotiated` |
| No certificate for the name | `tls::unknown_name_or_no_sni_fails_the_handshake` |
| WebSocket over HTTPS | `tls::websocket_over_https`; CI `acme` › "websocket over https" |
| Renewed while connected | `tls::swapping_certificates_keeps_open_connections` |
| Redirected | `tls::redirect_rules`; `https::terms_then_issue_then_https_and_redirect`; CI `acme`, `nginx` |
| Redirect off | `tls::redirect_off`; `https::terms_then_issue_then_https_and_redirect` (live) |
| Challenge request | `tls::redirect_rules` |
| Host without a certificate | `tls::redirect_rules` |
| HTTPS not available | `tls::redirect_rules`; `https::no_redirect_while_https_is_not_served` |
| Unclaimed name over HTTPS through nginx | CI `nginx` › "https through nginx" |
| nginx's own HTTPS site | CI `nginx` › "https through nginx" (`site.test` keeps its self-signed certificate) |
| New certificate behind nginx | CI `nginx` › "https through nginx" (no reload); `https::certificates_are_published_for_nginx_and_unpublished_on_rm` |

## daemon-lifecycle

| Scenario | Test |
| --- | --- |
| Port free | `daemon::run_serves_rules_and_stops_cleanly_on_sigterm` |
| Port held by nginx | `attach::behind_nginx_status_and_reclaim` |
| Port taken | `daemon::port_taken_waits_and_names_the_holder` |
| Port freed while waiting | `daemon::port_taken_waits_and_names_the_holder`; CI `systemd` |
| HTTPS port taken | `https::https_port_taken_keeps_http_serving` |
| A second proxlet | `daemon::second_run_says_already_running` |
| First start on a systemd host | `service.rs` `first_start_installs_runs_as_invoking_user_and_waits`; CI `systemd` |
| Already running | `service.rs` `start_when_already_running_changes_nothing` |
| Start on a taken port | `service.rs` `start_on_a_taken_port_succeeds_and_waits_for_it` |
| Start on a taken HTTPS port | `service.rs` `start_on_a_taken_https_port_warns_and_succeeds` |
| Start fails | `service.rs` `start_failure_shows_the_servers_error_and_stops_retrying`; CI `systemd` |
| Running standalone | `daemon::status_reports_running_details` |
| Running behind nginx | `attach::behind_nginx_status_and_reclaim` |
| HTTPS line | `https::terms_then_issue_then_https_and_redirect`; `status.rs` `https_line_follows_its_mode`; CI `acme` |
| Certificate expiring | `https::a_failing_renewal_keeps_the_old_certificate_and_is_reported` |
| Not running | `daemon::status_when_stopped_exits_nonzero_with_hint` |

## front-attach

| Scenario | Test |
| --- | --- |
| Unclaimed host reaches proxlet | CI `nginx` › "start attaches behind nginx" |
| Claimed host stays with nginx | CI `nginx` › "start attaches behind nginx" |
| WebSocket through nginx | CI `nginx` › "websocket through nginx" |
| HTTPS block installed | `service.rs` `https_block_and_publish_dir_with_a_tls_nginx_then_stop_removes_them`; `proxlet-front/tests/nginx_layouts.rs` `plan_with_https_adds_a_tls_block_that_reads_proxlets_certificates`; CI `nginx` (`nginx -t` passes before any certificate) |
| nginx too old for HTTPS forwarding | `service.rs` `old_nginx_gets_only_the_http_block_and_a_warning`; `proxlet-front/src/nginx/mod.rs` `https_needs_a_new_enough_nginx_with_tls` |
| Take over with confirmation | `service.rs` `take_over_with_confirmation_then_stop_restores_everything` |
| Take over without a prompt | `service.rs` `take_over_without_a_prompt`; CI `nginx` |
| Declined | `service.rs` `declined_or_no_terminal_leaves_nginx_config_unchanged`, `declining_keeps_both_defaults_and_writes_nothing`; `nginx_layouts.rs` `declining_only_skips_the_port_that_has_a_default`; `attach::run_without_a_terminal_keeps_nginxs_default_server` |
| Default on port 443 too | `service.rs` `one_question_names_the_default_servers_of_both_ports`; `nginx_layouts.rs` `default_on_port_443_too_moves_both_flags` |
| nginx starts while proxlet holds port 80 | `daemon::handoff_release_frees_the_port_and_reclaim_takes_it_back` (both ports); CI `nginx` |
| proxlet not running | `daemon::handoff_succeeds_when_proxlet_is_not_running` |
| nginx started by hand | `attach::a_bind_failure_in_nginxs_log_hands_the_port_over`; CI `nginx` › "nginx started by hand" |
| nginx fails for another reason | `ports.rs` `failed_front_start_is_reclaimed_quickly` (each port's owner) |
| nginx stopped | CI `nginx` › "nginx stops and starts as a service" (HTTPS served by proxlet after the stop) |
| Stop while attached | `service.rs` `https_block_and_publish_dir_with_a_tls_nginx_then_stop_removes_them`; CI `nginx` › "stop restores nginx exactly" |

## settings

| Scenario | Test |
| --- | --- |
| Defaults on a fresh install | `proxlet-core/src/settings.rs` `defaults` |
| Duration value | `cli::config_duration_value` |
| Attach port needs a restart | `cli::attach_port_needs_a_restart` |
| HTTPS port needs a restart | `cli::https_port_needs_a_restart` |
| Same port twice | `cli::ports_must_differ_and_nothing_changes`; `settings.rs` `ports_must_differ` |
| Redirect applies live | `https::terms_then_issue_then_https_and_redirect` |
| Invalid ACME server | `cli::acme_server_values` |

## Not yet verified

- The CI jobs `acme` (new) and `nginx` (extended with HTTPS), and the Pebble
  crate test they run (task 3.2), have not run yet. They run on the first push
  of this change.
