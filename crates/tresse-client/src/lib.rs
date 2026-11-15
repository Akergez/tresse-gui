//! Tresse on the filesystem, for the interface to drive.
//!
//! The engine is `tresse-lib` and the transports are `rdx-sync`'s: this
//! crate is the filesystem client around them, adapted from `tresse-cli`
//! the way Money Manager's `money-tresse` is. Its configuration, repository
//! layout, locking, file index and operation journal stay compatible with
//! the CLI, so one folder can be synced from this application and read by
//! the terminal.
//!
//! The filesystem is reached through a vault seam rather than `std::fs`
//! directly: ordinary paths on a desktop, Android's storage access framework
//! on a phone. That is what lets the person pick any folder on either.
//!
//! Nothing is implemented yet: stage S1 brings the client.
