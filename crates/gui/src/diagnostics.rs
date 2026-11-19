#[derive(Default)]
pub struct RenderedDiagnostic {
    base: String,
    rendered: String,
}
impl RenderedDiagnostic {
    pub fn apply(&mut self, current: &str, local: &str) -> String {
        if current != self.rendered {
            self.base = current.to_owned();
        }
        self.rendered = match (self.base.is_empty(), local.is_empty()) {
            (_, true) => self.base.clone(),
            (true, false) => local.to_owned(),
            (false, false) => format!("{}\n{local}", self.base),
        };
        self.rendered.clone()
    }
}
#[derive(Default)]
pub struct DiagnosticRender {
    pub error: RenderedDiagnostic,
    pub status: RenderedDiagnostic,
}
#[cfg(test)]
mod tests {
    use super::RenderedDiagnostic;
    #[test]
    fn repeated_render_preserves_base_without_duplicate_diagnostics() {
        let mut state = RenderedDiagnostic::default();
        let first = state.apply("input failed", "local failure");
        assert_eq!(state.apply(&first, "local failure"), first);
        let switched = state.apply(&first, "本地诊断");
        assert_eq!(switched.lines().count(), 2);
        assert_eq!(state.apply(&switched, "本地诊断"), switched);
        assert_eq!(
            state.apply("new input error", "本地诊断").lines().count(),
            2
        );
        assert_eq!(state.apply("", "本地诊断").lines().count(), 1);
    }
}
