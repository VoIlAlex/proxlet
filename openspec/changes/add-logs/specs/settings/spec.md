## MODIFIED Requirements

### Requirement: Available settings

proxlet SHALL provide these settings with these defaults: `listen.address`
(`0.0.0.0`), `listen.http_port` (`80`), `listen.https_port` (`443`),
`static.listing` (`on`), `static.dotfiles` (`off`), `proxy.connect_timeout`
(`5s`), `proxy.timeout` (`60s`), `attach.enabled` (`on`), `attach.port`
(`47080`), `tls.redirect` (`on`), `cert.server` (`production`), `log.access`
(`on`), `log.max_size` (`100MB`) and `log.max_age` (`7d`).

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

#### Scenario: Log limits apply live

- **WHEN** the server is running and the user runs `proxlet config log.max_size 20MB`
- **THEN** the oldest log files beyond 20 MB are deleted at the next rotation, with no restart

### Requirement: Setting values

Booleans SHALL accept `on`/`off` and `true`/`false`; durations a number with
`ms`, `s`, `m`, `h` or `d`; sizes a number with `KB`, `MB` or `GB` (powers of
1024); `cert.server` `production`, `staging` or an `https://` URL. The HTTP,
HTTPS and attach ports SHALL all differ, and `log.max_size` SHALL be at least
`10MB`, the size of one log file.

#### Scenario: Duration value

- **WHEN** the user runs `proxlet config proxy.timeout 2m`
- **THEN** upstream responses are allowed up to two minutes before a 504

#### Scenario: Days and sizes

- **WHEN** the user runs `proxlet config log.max_age 30d` and `proxlet config log.max_size 1GB`
- **THEN** both are saved and `proxlet config` shows `30d` and `1GB`

#### Scenario: Size too small

- **WHEN** the user runs `proxlet config log.max_size 1MB`
- **THEN** the command exits non-zero, says the size must be at least `10MB`, and the setting is unchanged

#### Scenario: Same port twice

- **WHEN** the user runs `proxlet config listen.https_port 80` and `listen.http_port` is `80`
- **THEN** the command exits non-zero, says the HTTPS port must differ from the HTTP port, and the setting is unchanged

#### Scenario: Invalid ACME server

- **WHEN** the user runs `proxlet config cert.server http://acme.local/dir`
- **THEN** the command exits non-zero and says the value must be `production`, `staging` or an `https://` URL
