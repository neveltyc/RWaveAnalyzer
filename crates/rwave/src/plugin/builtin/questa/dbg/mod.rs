// Copyright (c) 2026 neveltyc
// released under the MIT License (see LICENSE)

//! Reading Questa's post-simulation debug database.
//!
//! `vopt -debugdb` writes connectivity into a `.dbg` beside the waveform and a
//! per-design-unit database under `<lib>/_dbcontainer/<opt>/`. Between them
//! they hold everything `trace` needs — drivers, loads, their operands and
//! control dependencies, each with a source location — which is more than
//! QuestaSim's own post-simulation commands will print.

pub mod design;
pub mod open;
pub mod schema;

pub use design::Design;
pub use open::{Db, Kind};
