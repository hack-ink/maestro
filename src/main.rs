//! Maestro binary entrypoint.

#![allow(unused_crate_dependencies)]

use color_eyre::Result;

fn main() -> Result<()> {
	maestro::run()
}
