//! Compiles the .slint files into Rust; `zwt_app` includes the result with `slint::include_modules!()`.

fn main() -> Result<(), slint_build::CompileError> {
    // A fixed style keeps the look (and the screenshots) the same whatever SLINT_STYLE says.
    let mut config = slint_build::CompilerConfiguration::new().with_style("fluent".into());
    // The UI tests find elements through the testing backend's ElementHandle, which needs the element
    // debug info. Release builds leave it out, unless SLINT_EMIT_DEBUG_INFO=1 is set.
    if std::env::var("PROFILE").is_ok_and(|profile| profile != "release") {
        config = config.with_debug_info(true);
    }
    slint_build::compile_with_config("ui/app.slint", config)
}
