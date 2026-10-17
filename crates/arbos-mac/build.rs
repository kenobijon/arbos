fn main() {
    // The build number is a compile-time constant (see `update::current_version`);
    // a change in the environment must rebuild the crate.
    println!("cargo:rerun-if-env-changed=ARBOS_BUILD");
    // libwebrtc (calls v1) is a static library full of Objective-C categories
    // (`NSString+StdString`…). Without -ObjC the linker drops them and the app
    // aborts in a static initialiser on launch. webrtc-sys asks for the flag,
    // but Cargo only honours link args from the final binary's own build script.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-link-arg-bins=-ObjC");
    }
}
