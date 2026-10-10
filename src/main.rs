//! Native desktop entry point: thin wrapper over the `nova` library crate
//! (catalog UI + libmpv2 in-app player).

// Launch the Windows desktop app without allocating a console window.
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

// Linux desktop: allocate through jemalloc instead of glibc malloc. The
// Discover scrolling workload (bursts of multi-MB image buffers interleaved
// with small allocations) fragments glibc arenas so RSS keeps climbing
// after the data is freed; jemalloc's arenas + decay-based background purge
// return such pages on their own.
#[cfg(target_os = "linux")]
#[global_allocator]
static ALLOC: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(feature = "desktop")]
    if nova::startup_bench::initialize(std::time::Instant::now())? {
        return Ok(());
    }
    nova::app::run()
}
