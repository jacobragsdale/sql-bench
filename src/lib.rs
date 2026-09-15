//! sql-bench: a terminal workbench for SQL Server and Oracle.
//!
//! The crate is split the way [`CLAUDE.md`](../CLAUDE.md) describes: [`app`]
//! is pure state, [`ui`] renders it, [`run`] owns the terminal and the
//! threads, [`db`] talks to the databases, and [`cli`] and [`config`] say
//! what a run is asked to do.

pub mod app;
pub mod cli;
pub mod config;
pub mod db;
pub mod run;
pub mod trace;
pub mod ui;
