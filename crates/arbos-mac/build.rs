fn main() {
    // The build number is a compile-time constant (see `update::current_version`);
    // a change in the environment must rebuild the crate.
    println!("cargo:rerun-if-env-changed=ARBOS_BUILD");
}
