//! The packaged release version reaches the binaries through an environment
//! variable, so a dated release does not need a commit editing every crate.
fn main() {
    println!("cargo:rerun-if-env-changed=TEAMUP_SHIFT_SYNC_VERSION");
}
