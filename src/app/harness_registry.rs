use crate::harness::integration::HarnessIntegration;
use crate::harness::kind::HarnessKind;
use crate::integrations::{
    claude::ClaudeIntegration, codex::CodexIntegration, opencode::OpenCodeIntegration,
};

pub trait HarnessRegistry {
    fn all(&self) -> Vec<Box<dyn HarnessIntegration>>;

    fn get(&self, kind: HarnessKind) -> Option<Box<dyn HarnessIntegration>> {
        self.all()
            .into_iter()
            .find(|integration| integration.kind() == kind)
    }

    fn resolve_kind(&self, id: &str) -> Option<HarnessKind> {
        self.all()
            .into_iter()
            .map(|integration| integration.kind())
            .find(|kind| kind.id() == id)
    }

    fn supported_ids(&self) -> Vec<&'static str> {
        self.all()
            .into_iter()
            .map(|integration| integration.kind().id())
            .collect()
    }

    fn require_kind(&self, id: &str) -> anyhow::Result<HarnessKind> {
        self.resolve_kind(id).ok_or_else(|| {
            anyhow::anyhow!(
                "unsupported harness {id}; supported harnesses: {}",
                self.supported_ids().join(", ")
            )
        })
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct BuiltInHarnessRegistry;

impl HarnessRegistry for BuiltInHarnessRegistry {
    fn all(&self) -> Vec<Box<dyn HarnessIntegration>> {
        vec![
            Box::new(CodexIntegration),
            Box::new(ClaudeIntegration),
            Box::new(OpenCodeIntegration),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_exposes_the_v1_harnesses() {
        let kinds = BuiltInHarnessRegistry
            .all()
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
