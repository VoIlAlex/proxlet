## MODIFIED Requirements

### Requirement: Port check on start

The server SHALL listen on its HTTP port when the port is free. When nginx holds
it and attach is enabled, the server SHALL attach behind nginx. When any other
process holds it, the server SHALL keep running, log an error naming the port and
the holder, and retry until the port is free. A second proxlet SHALL still exit
non-zero.

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

#### Scenario: Start fails

- **WHEN** the user runs `proxlet start` and the server cannot start because its config file is invalid
- **THEN** the command exits non-zero and shows the server's error, including the file and line, not only a generic service failure

### Requirement: Status

`proxlet status` SHALL report whether the server is running. When running it
SHALL show the mode (`standalone`, `attached (nginx)` or `waiting for port <n>
(held by <name>)`), the listening address, how it is run, the uptime and the
number of rules. It SHALL exit zero when running and non-zero when not.

#### Scenario: Running standalone

- **WHEN** the server is running and the user runs `proxlet status`
- **THEN** the output shows running, standalone, the listening address, uptime and rule count, and the command exits zero

#### Scenario: Running behind nginx

- **WHEN** the server is attached behind nginx and the user runs `proxlet status`
- **THEN** the output shows `mode    attached (nginx)` and `listen  http://127.0.0.1:<attach.port> (behind nginx on port 80)`, and the command exits zero

#### Scenario: Not running

- **WHEN** the server is not running and the user runs `proxlet status`
- **THEN** the output says proxlet is not running and how to start it, and the command exits non-zero
