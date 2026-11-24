# settings Specification

## Purpose

Defines the `proxlet config` command and the settings that control how proxlet
listens, proxies and serves files.

## Requirements

### Requirement: Listing and reading settings

`proxlet config` SHALL list every setting with its current value and mark values
that differ from the default. `proxlet config <key>` SHALL print that setting's
value only. An unknown key SHALL exit non-zero and suggest the closest known key.

#### Scenario: List settings

- **WHEN** the user runs `proxlet config`
- **THEN** every setting is shown with its current value

#### Scenario: Read one setting

- **WHEN** the user runs `proxlet config static.listing` and it has not been changed
- **THEN** the output is `on`

#### Scenario: Unknown key

- **WHEN** the user runs `proxlet config static.listng`
- **THEN** the command exits non-zero and suggests `static.listing`

### Requirement: Changing a setting

`proxlet config <key> <value>` SHALL validate the value, persist it, and apply it
to a running server. An invalid value SHALL exit non-zero, state the accepted
values, and change nothing. Settings that cannot change while running SHALL be
saved and the output SHALL say a restart is needed.

#### Scenario: Change applies live

- **WHEN** the server is running and the user runs `proxlet config static.listing off`
- **THEN** the next request for a directory without an index file receives 404, with no restart

#### Scenario: Invalid value

- **WHEN** the user runs `proxlet config static.listing maybe`
- **THEN** the command exits non-zero, says the value must be `on` or `off`, and the setting is unchanged

#### Scenario: Restart needed

- **WHEN** the server is running and the user runs `proxlet config listen.http_port 8080`
- **THEN** the value is saved and the output says it takes effect after a restart

### Requirement: Available settings

proxlet SHALL provide these settings with these defaults: `listen.address`
(`0.0.0.0`), `listen.http_port` (`80`), `listen.https_port` (`443`),
`static.listing` (`on`), `static.dotfiles` (`off`), `proxy.connect_timeout`
(`5s`), `proxy.timeout` (`60s`), `attach.enabled` (`on`), `attach.port`
(`47080`), `tls.redirect` (`on`) and `cert.server` (`production`).

#### Scenario: Defaults on a fresh install

- **WHEN** no setting has been changed and the user runs `proxlet config`
- **THEN** the values shown are the defaults above and none is marked as changed

#### Scenario: Attach port needs a restart

- **WHEN** the user runs `proxlet config attach.port 47081` while proxlet is running
- **THEN** the value is saved and the output says it takes effect when proxlet is restarted

#### Scenario: HTTPS port needs a restart

- **WHEN** the user runs `proxlet config listen.https_port 8443` while proxlet is running
- **THEN** the value is saved and the output says it takes effect when proxlet is restarted

#### Scenario: Redirect applies live

- **WHEN** the server is running and the user runs `proxlet config tls.redirect off`
- **THEN** the next plain HTTP request for a host with a certificate is served without a redirect, with no restart

### Requirement: Setting values

Boolean settings SHALL accept `on`/`off` and `true`/`false`; durations SHALL
accept a number with an `ms`, `s` or `m` suffix; `cert.server` SHALL accept
`production`, `staging` or an `https://` URL. `listen.https_port`,
`listen.http_port` and `attach.port` SHALL all differ.

#### Scenario: Duration value

- **WHEN** the user runs `proxlet config proxy.timeout 2m`
- **THEN** upstream responses are allowed up to two minutes before a 504

#### Scenario: Same port twice

- **WHEN** the user runs `proxlet config listen.https_port 80` and `listen.http_port` is `80`
- **THEN** the command exits non-zero, says the HTTPS port must differ from the HTTP port, and the setting is unchanged

#### Scenario: Invalid ACME server

- **WHEN** the user runs `proxlet config cert.server http://acme.local/dir`
- **THEN** the command exits non-zero and says the value must be `production`, `staging` or an `https://` URL

### Requirement: Setting completion

Shell completion SHALL offer setting names for `proxlet config`, and the accepted
values for settings with a fixed set of values.

#### Scenario: Complete a key

- **WHEN** the user presses TAB after `proxlet config static.`
- **THEN** `static.listing` and `static.dotfiles` are offered

#### Scenario: Complete a value

- **WHEN** the user presses TAB after `proxlet config static.listing `
- **THEN** `on` and `off` are offered
