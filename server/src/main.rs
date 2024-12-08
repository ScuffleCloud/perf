#![cfg_attr(all(coverage_nightly, test), feature(coverage_attribute))]
// disable coverage for now.
#![cfg_attr(all(coverage_nightly, test), coverage(off))]

use crate::global::Global;

mod config;
mod global;

scuffle_bootstrap::main! {
	Global {
		scuffle_signal::SignalSvc,
	}
}
