# xattr 0.2.3, patched

This is [xattr](https://github.com/Stebalien/xattr) 0.2.3 with one change to
what it does, in `src/sys/mod.rs`: on Linux and Android the "no such
attribute" error is taken from `libc::ENODATA` instead of `libc::ENOATTR`.
`src/lib.rs` also silences the crate's compiler warnings, and the tests and
CI files are left out.

GPUI 0.2.2 depends on this version through `zed-async-tar`. It names
`libc::ENOATTR`, which current releases of `libc` no longer define for Linux,
so without the change Scrollback does not compile there. Linux has always
reported a missing attribute as `ENODATA`, so the value is the same one the
old constant held.

The workspace `Cargo.toml` points Cargo at this copy with `[patch.crates-io]`.
Remove both once GPUI depends on a newer xattr.
