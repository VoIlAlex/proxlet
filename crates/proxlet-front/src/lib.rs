//! Front-server integrations for attach mode.
//!
//! Each supported front server (nginx first) lives in its own module behind one
//! trait, so adding another server such as Apache never touches the engine or CLI.
