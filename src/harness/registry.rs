use crate::harness::integration::HarnessIntegration;
use crate::integrations::{
    claude::ClaudeIntegration, codex::CodexIntegration, opencode::OpenCodeIntegration,
};

pub fn all() -> Vec<Box<dyn HarnessIntegration>> {
    vec![
        Box::new(CodexIntegration),
        Box::new(ClaudeIntegration),
        Box::new(OpenCodeIntegration),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::kind::HarnessKind;

    #[test]
    fn registry_exposes_the_v1_harnesses() {
        let kinds = all()
            .into_iter()
            .map(|integration| integration.kind())
            .collect::<Vec<_>>();

        assert_eq!(
            kinds,
            vec![
                HarnessKind::Codex,
                HarnessKind::Claude,
                HarnessKind::OpenCode
            ]
        );
    }
}
