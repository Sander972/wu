fn main() {
    // rustc's generated .def exports DllGetClassObject without PRIVATE; that only adds it to the unused import library, so silence LNK4104.
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        println!("cargo:rustc-link-arg=/IGNORE:4104");
    }
}
