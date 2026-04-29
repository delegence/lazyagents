#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DriftReport {
    pub items: Vec<DriftItem>,
}

impl DriftReport {
    pub fn clean() -> Self {
        Self { items: Vec::new() }
    }

    pub fn is_clean(&self) -> bool {
        self.items.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriftItem {
    pub surface: String,
    pub detail: String,
}
