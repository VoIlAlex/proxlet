## MODIFIED Requirements

### Requirement: Available settings

proxlet SHALL provide these settings with these defaults: `listen.address`
(`0.0.0.0`), `listen.http_port` (`80`), `static.listing` (`on`),
`static.dotfiles` (`off`), `proxy.connect_timeout` (`5s`), `proxy.timeout`
(`60s`), `attach.enabled` (`on`) and `attach.port` (`47080`). Boolean settings
SHALL accept `on`/`off` and `true`/`false`; durations SHALL accept a number with
an `ms`, `s` or `m` suffix.

#### Scenario: Defaults on a fresh install

- **WHEN** no setting has been changed and the user runs `proxlet config`
- **THEN** the values shown are the defaults above and none is marked as changed

#### Scenario: Duration value

- **WHEN** the user runs `proxlet config proxy.timeout 2m`
- **THEN** upstream responses are allowed up to two minutes before a 504

#### Scenario: Attach port needs a restart

- **WHEN** the user runs `proxlet config attach.port 47081` while proxlet is running
- **THEN** the value is saved and the output says it takes effect when proxlet is restarted
