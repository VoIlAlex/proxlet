## MODIFIED Requirements

### Requirement: Available settings

proxlet SHALL provide these settings with these defaults: `listen.address`
(`0.0.0.0`), `listen.http_port` (`80`), `listen.https_port` (`443`),
`static.listing` (`on`), `static.dotfiles` (`off`), `proxy.connect_timeout`
(`5s`), `proxy.timeout` (`60s`), `attach.enabled` (`on`), `attach.port`
(`47080`), `tls.redirect` (`on`) and `cert.server` (`production`). Boolean
settings SHALL accept `on`/`off` and `true`/`false`; durations SHALL accept a
number with an `ms`, `s` or `m` suffix. `cert.server` SHALL accept
`production`, `staging` or an `https://` URL. `listen.https_port` SHALL differ
from `listen.http_port` and `attach.port`.

#### Scenario: Defaults on a fresh install

- **WHEN** no setting has been changed and the user runs `proxlet config`
- **THEN** the values shown are the defaults above and none is marked as changed

#### Scenario: Duration value

- **WHEN** the user runs `proxlet config proxy.timeout 2m`
- **THEN** upstream responses are allowed up to two minutes before a 504

#### Scenario: Attach port needs a restart

- **WHEN** the user runs `proxlet config attach.port 47081` while proxlet is running
- **THEN** the value is saved and the output says it takes effect when proxlet is restarted

#### Scenario: HTTPS port needs a restart

- **WHEN** the user runs `proxlet config listen.https_port 8443` while proxlet is running
- **THEN** the value is saved and the output says it takes effect when proxlet is restarted

#### Scenario: Same port twice

- **WHEN** the user runs `proxlet config listen.https_port 80` and `listen.http_port` is `80`
- **THEN** the command exits non-zero, says the HTTPS port must differ from the HTTP port, and the setting is unchanged

#### Scenario: Redirect applies live

- **WHEN** the server is running and the user runs `proxlet config tls.redirect off`
- **THEN** the next plain HTTP request for a host with a certificate is served without a redirect, with no restart

#### Scenario: Invalid ACME server

- **WHEN** the user runs `proxlet config cert.server http://acme.local/dir`
- **THEN** the command exits non-zero and says the value must be `production`, `staging` or an `https://` URL
