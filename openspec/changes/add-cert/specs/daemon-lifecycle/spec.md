## MODIFIED Requirements

### Requirement: Port check on start

The server SHALL handle its HTTP port (`listen.http_port`) and its HTTPS port
(`listen.https_port`) independently. For each port it SHALL listen on it when
the port is free. When nginx holds it and attach is enabled, the server SHALL
attach behind nginx for that port. When any other process holds it, the server
SHALL keep running, log an error naming the port and the holder, and retry until
the port is free. A taken HTTPS port SHALL NOT affect the HTTP port, and the
reverse. A second proxlet SHALL still exit non-zero.

#### Scenario: Port free

- **WHEN** the HTTP port is free and the server is started
- **THEN** it begins listening on that port

#### Scenario: Port held by nginx

- **WHEN** nginx is listening on the HTTP port and the server is started
- **THEN** the server listens on `127.0.0.1:<attach.port>` and `proxlet status` shows `mode    attached (nginx)`

#### Scenario: Port taken

- **WHEN** another process is listening on the HTTP port and `proxlet run` is started
- **THEN** the server keeps running, logs `port 80 is already in use by <name> (pid <pid>); waiting for it to be free`, and `proxlet status` shows `mode    waiting for port 80 (held by <name>)`

#### Scenario: Port freed while waiting

- **GIVEN** the server is waiting for its HTTP port
- **WHEN** the holder stops listening
- **THEN** the server listens on the port within 5 seconds and serves its rules

#### Scenario: HTTPS port taken

- **WHEN** another process listens on port 443 and port 80 is free, and the server is started
- **THEN** the server serves plain HTTP on port 80, logs that port 443 is in use by `<name>`, and `proxlet status` shows `https   waiting for port 443 (held by <name>)`

#### Scenario: A second proxlet

- **WHEN** proxlet is already running and `proxlet run` is started again
- **THEN** it exits non-zero and says proxlet is already running

### Requirement: Start as a system service

`proxlet start` SHALL install proxlet as a system service through the host's
service manager, enable it so it starts at boot, start it, and report success
only once the server is accepting requests. Running it when the service is
already running SHALL succeed and say so.

#### Scenario: First start on a systemd host

- **WHEN** the user runs `proxlet start` on a Linux host with systemd
- **THEN** a proxlet service is installed, enabled and running, and the command prints the listening address and exits zero

#### Scenario: Already running

- **WHEN** the service is running and the user runs `proxlet start`
- **THEN** the command says proxlet is already running and exits zero

#### Scenario: Start on a taken port

- **WHEN** a process other than nginx holds port 80 and the user runs `sudo proxlet start`
- **THEN** the service is running, the command warns `waiting for port 80, held by <name> (pid <pid>)`, and exits zero

#### Scenario: Start on a taken HTTPS port

- **WHEN** a process other than nginx holds port 443 and the user runs `sudo proxlet start`
- **THEN** the service is running, the command warns `waiting for port 443, held by <name> (pid <pid>)`, and exits zero

#### Scenario: Start fails

- **WHEN** the user runs `proxlet start` and the server cannot start because its config file is invalid
- **THEN** the command exits non-zero and shows the server's error, including the file and line, not only a generic service failure

### Requirement: Status

`proxlet status` SHALL report whether the server is running. When running it
SHALL show the HTTP port's mode (`standalone`, `attached (nginx)` or `waiting
for port <n> (held by <name>)`), the listening address, the HTTPS port's state
on an `https` line (its address and the number of certificates, `attached
(nginx)`, or `waiting for port <n> (held by <name>)`), how it is run, the
uptime and the number of rules. It SHALL warn about each certificate with 14
days or less left or whose renewal is failing. It SHALL exit zero when running
and non-zero when not.

#### Scenario: Running standalone

- **WHEN** the server is running and the user runs `proxlet status`
- **THEN** the output shows running, standalone, the listening address, uptime and rule count, and the command exits zero

#### Scenario: Running behind nginx

- **WHEN** the server is attached behind nginx and the user runs `proxlet status`
- **THEN** the output shows `mode    attached (nginx)` and `listen  http://127.0.0.1:<attach.port> (behind nginx on port 80)`, and the command exits zero

#### Scenario: HTTPS line

- **GIVEN** proxlet holds port 443 and has two certificates
- **WHEN** the user runs `proxlet status`
- **THEN** the output includes `https   https://0.0.0.0:443 (2 certificates)`

#### Scenario: Certificate expiring

- **GIVEN** the certificate for `example.com` has 10 days left and its renewal is failing
- **WHEN** the user runs `proxlet status`
- **THEN** the output warns that `example.com` expires in 10 days, gives the renewal error, and the command still exits zero

#### Scenario: Not running

- **WHEN** the server is not running and the user runs `proxlet status`
- **THEN** the output says proxlet is not running and how to start it, and the command exits non-zero
