use std::path::PathBuf;

fn main() {
    // The Swift shim is built next to this crate by ../build.sh, so the
    // search path is relative to the manifest rather than absolute.
    let shim_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crate has a parent directory")
        .to_path_buf();
    println!("cargo:rustc-link-search=native={}", shim_dir.display());
    println!("cargo:rustc-link-lib=static=outloud_fm");

    // The Swift runtime ships with the OS, but a Rust binary carries no
    // Swift rpath, so libswift_Concurrency.dylib is not found at load time
    // and the process dies in dyld before main(). Adding the OS Swift
    // directory as an rpath is what a real integration must do too; finding
    // that out is half the value of this spike.
    println!("cargo:rustc-link-search=native=/usr/lib/swift");
    println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");

    for framework in ["Foundation", "FoundationModels"] {
        println!("cargo:rustc-link-lib=framework={framework}");
    }
}
