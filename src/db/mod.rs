//! The databases: the value model every backend reports in, and (from T2.1)
//! the connection handle whose worker thread owns the driver so the event
//! loop never waits on a network.

pub mod model;
