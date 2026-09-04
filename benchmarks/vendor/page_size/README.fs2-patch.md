# fs2 benchmark patch

This directory contains `page_size` 0.6.0 with one dependency-level change for
the fs2 benchmark workspace: its Windows system-information calls use
`windows-sys` 0.61.2 instead of `winapi` 0.3.9.

The public API, one-time caching, Unix implementations, and Windows calls remain
otherwise unchanged. The override is limited to benchmark tooling and is
excluded from the published `fs2` package.

Upstream tracking: https://github.com/Elzair/page_size_rs/issues/10
