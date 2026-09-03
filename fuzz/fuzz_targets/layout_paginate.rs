#![no_main]
//! New target (D5.5, issue #90): random paragraph/table/section trees
//! straight into the paginator, with a page-count bound standing in for a
//! termination watchdog. See `engine_fuzz::run_layout_paginate` /
//! `engine_fuzz::layout_gen`.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    engine_fuzz::run_layout_paginate(data);
});
