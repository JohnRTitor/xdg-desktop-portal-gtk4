pub mod core;
pub mod gui;
pub mod logging;
pub mod portals;

// Allocation counting, used to validate memory-layout changes against measured
// allocation counts rather than by inspection. Test-only: the shim is never
// linked into the shipped binary or the integration-test binaries.
#[cfg(test)]
mod alloc_probe;
#[cfg(test)]
#[global_allocator]
static COUNTING_ALLOC: alloc_probe::CountingAlloc = alloc_probe::CountingAlloc;

rust_i18n::i18n!();
