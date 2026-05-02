pub mod claude;
pub mod codex;
pub mod gemini;
pub mod opencode;
pub mod pi;

#[cfg(test)]
pub mod test_suite;

use crate::harness::integration::HarnessIntegration;
use crate::harness::kind::HarnessKind;

pub fn built_in_integrations() -> Vec<Box<dyn HarnessIntegration>> {
    vec![
        Box::new(codex::CodexIntegration),
        Box::new(claude::ClaudeIntegration),
        Box::new(gemini::GeminiIntegration),
        Box::new(opencode::OpenCodeIntegration),
        Box::new(pi::PiIntegration),
    ]
}

pub fn integration_for_kind(kind: HarnessKind) -> Box<dyn HarnessIntegration> {
    built_in_integrations()
        .into_iter()
        .find(|integration| integration.kind() == kind)
        .expect("built-in harness kind is not registered")
}
