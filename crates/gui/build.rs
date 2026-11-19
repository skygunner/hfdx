fn main() -> Result<(), slint_build::CompileError> {
    let config = slint_build::CompilerConfiguration::new()
        .with_default_translation_context(slint_build::DefaultTranslationContext::None)
        .with_bundled_translations("ui/translations");
    slint_build::compile_with_config("ui/app.slint", config)
}
