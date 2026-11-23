## Purpose

Defines how the proxlet server is started, stopped and inspected, how it is
installed as a system service, and what it checks before it starts listening.

## ADDED Requirements

### Requirement: Foreground run

`proxlet run` SHALL run the server in the foreground until interrupted. On an
interrupt or terminate signal it SHALL stop accepting connections, let in-flight
requests finish within a bounded grace period, and exit zero.

#### Scenario: Run and interrupt

- **WHEN** the user runs `proxlet run` and later presses Ctrl-C
- **THEN** the server serves requests in between, then shuts down cleanly and exits zero

### Requirement: Port check on start

Before listening, the server SHALL check that its HTTP port is free. If the port
is taken it SHALL NOT start, and SHALL exit non-zero with an error naming the port
and, where it can be determined, the process holding it.

#### Scenario: Port free

- **WHEN** the HTTP port is free and the server is started
- **THEN** it begins listening on that port

#### Scenario: Port taken

- **WHEN** another process is listening on the HTTP port and the server is started
- **THEN** it exits non-zero and the error names the port and the process that holds it

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

#### Scenario: Start fails

- **WHEN** the user runs `proxlet start` and the server cannot start because its port is taken
- **THEN** the command exits non-zero and shows the server's error, not only a generic service failure

### Requirement: Unsupported service manager

On a host with no supported service manager, `proxlet start` SHALL exit
non-zero, say which service managers are supported, and point to `proxlet run`.
`proxlet stop` SHALL do the same unless a foreground server is running.

#### Scenario: Start on macOS

- **WHEN** the user runs `proxlet start` on macOS
- **THEN** the command exits non-zero, says no supported service manager was found, and suggests `proxlet run`

#### Scenario: Stop on macOS with nothing running

- **WHEN** no proxlet is running and the user runs `proxlet stop` on macOS
- **THEN** the command exits non-zero, says proxlet is not running and which service managers are supported, and suggests `proxlet run`

### Requirement: Root is needed only to install

`proxlet start` and `proxlet stop` SHALL tell the user to re-run them with
`sudo` when they need to install, start or stop the system service without
root, and SHALL change nothing in that case.

#### Scenario: Start without root

- **WHEN** a non-root user runs `proxlet start` on a systemd host and proxlet is not running
- **THEN** the command exits non-zero, suggests `sudo proxlet start`, and installs nothing

### Requirement: Stop the service

`proxlet stop` SHALL stop the service gracefully and disable it so it does not
start again at boot. Rules and settings SHALL be kept. Running it when the
service is not running SHALL succeed and say so.

#### Scenario: Stop a running service

- **WHEN** the service is running and the user runs `proxlet stop`
- **THEN** the server stops accepting requests, the service is disabled, and `proxlet ls` still lists the rules

#### Scenario: Already stopped

- **WHEN** the service is not running and the user runs `proxlet stop`
- **THEN** the command says proxlet is not running and exits zero

#### Scenario: Stop a foreground server

- **WHEN** proxlet was started with `proxlet run` and the user runs `proxlet stop`
- **THEN** that server shuts down gracefully, and the command says so and exits zero

### Requirement: Status

`proxlet status` SHALL report whether the server is running. When running it
SHALL show the mode (standalone), the listening address, how it is run (system
service or foreground), the uptime and the number of rules. It SHALL exit zero
when the server is running and non-zero when it is not.

#### Scenario: Running standalone

- **WHEN** the server is running and the user runs `proxlet status`
- **THEN** the output shows running, standalone, the listening address, uptime and rule count, and the command exits zero

#### Scenario: Not running

- **WHEN** the server is not running and the user runs `proxlet status`
- **THEN** the output says proxlet is not running and how to start it, and the command exits non-zero
