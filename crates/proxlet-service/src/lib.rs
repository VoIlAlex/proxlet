//! System service manager integrations.
//!
//! `proxlet start` / `proxlet stop` go through one trait; systemd is the first
//! implementation, and other managers (launchd, `OpenRC`, ...) plug in beside it.
