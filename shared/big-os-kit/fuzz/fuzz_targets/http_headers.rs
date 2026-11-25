#![no_main]
// SPDX-License-Identifier: MIT
//! curl's header blocks, as a server sends them, never panic the reader.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| big_os_kit::http_client::fuzz_read_headers(data));
