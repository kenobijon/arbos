fn main() {
    // The build number and default update channel are compile-time constants
    // (see `update::current_version` / `update::feed_url`); a change in either
    // environment variable must rebuild the crate so a stable tag build does
    // not keep an edge feed baked in from a previous compile on the same runner.
    println!("cargo:rerun-if-env-changed=ARBOS_BUILD");
    println!("cargo:rerun-if-env-changed=ARBOS_DEFAULT_UPDATE_CHANNEL");
    // libwebrtc (calls v1) is a static library full of Objective-C categories
    // (`NSString+StdString`…). Without -ObjC the linker drops them and the app
    // aborts in a static initialiser on launch. webrtc-sys asks for the flag,
    // but Cargo only honours link args from the final binary's own build script.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-link-arg-bins=-ObjC");
    }
}
