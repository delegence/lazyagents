use std::fmt;

use anyhow::{Context, Result};
use serde_json::{json, Value};

use super::Client;

#[derive(Clone, Copy)]
pub enum ConfigKind {
    Model,
    Thinking,
}

#[derive(Clone, Debug)]
pub struct ConfigChoice {
    pub value: String,
    pub label: String,
    pub description: Option<String>,
}

impl fmt::Display for ConfigChoice {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = crate::markdown::sanitize(&self.label);
        match &self.description {
            Some(description) if !description.is_empty() => {
                let description = crate::markdown::sanitize(description);
                write!(formatter, "{label} - {description}")
            }
            _ => formatter.write_str(&label),
        }
    }
}

#[derive(Clone, Debug)]
pub struct SelectConfigOption {
    category: Option<String>,
    pub id: String,
    pub current_value: String,
    pub choices: Vec<ConfigChoice>,
}

impl SelectConfigOption {
    /// The index of the current choice, for a picker's starting position.
    pub fn cursor(&self) -> usize {
        self.choices
            .iter()
            .position(|choice| choice.value == self.current_value)
            .unwrap_or_default()
    }
}

impl Client {
    pub fn select_config_option(&self, kind: ConfigKind) -> Option<&SelectConfigOption> {
        self.config_options
            .iter()
            .find(|option| matches_kind(kind, &option.id, option.category.as_deref()))
    }

    pub async fn set_config_option(&mut self, id: &str, value: &str) -> Result<()> {
        let session_id = self.session_id.clone();
        let response = self
            .request(
                "session/set_config_option",
                json!({"sessionId": session_id, "configId": id, "value": value}),
            )
            .await
            .with_context(|| format!("could not set ACP option {id:?}"))?;
        self.update_config_options(&response);
        if response.get("configOptions").is_none() {
            if let Some(option) = self
                .config_options
                .iter_mut()
                .find(|option| option.id == id)
            {
                option.current_value = value.to_owned();
            }
        }
        Ok(())
    }

    pub async fn set_config(&mut self, kind: ConfigKind, value: &str) -> Result<bool> {
        let Some(option) = self.select_config_option(kind) else {
            return Ok(false);
        };
        if option.current_value == value {
            return Ok(true);
        }
        if !option.choices.iter().any(|choice| choice.value == value) {
            return Ok(false);
        }
        let id = option.id.clone();
        self.set_config_option(&id, value).await?;
        Ok(true)
    }

    pub fn current_config_label(&self, kind: ConfigKind) -> Option<&str> {
        let option = self.select_config_option(kind)?;
        option
            .choices
            .iter()
            .find(|choice| choice.value == option.current_value)
            .map(|choice| choice.label.as_str())
            .or(Some(option.current_value.as_str()))
    }

    pub(super) fn update_config_options(&mut self, response: &Value) {
        if let Some(options) = response.get("configOptions").and_then(Value::as_array) {
            self.config_options = options.iter().filter_map(parse_config_option).collect();
        }
    }
}

fn matches_kind(kind: ConfigKind, id: &str, category: Option<&str>) -> bool {
    match kind {
        ConfigKind::Model => category == Some("model") || id == "model",
        ConfigKind::Thinking => {
            category == Some("thought_level")
                || ["reasoning", "thinking", "effort"]
                    .iter()
                    .any(|hint| id.contains(hint))
        }
    }
}

fn parse_config_option(option: &Value) -> Option<SelectConfigOption> {
    if option.get("type")?.as_str()? != "select" {
        return None;
    }
    let mut choices: Vec<_> = option
        .get("options")?
        .as_array()?
        .iter()
        .flat_map(flatten_choices)
        .collect();
    if choices.is_empty() {
        return None;
    }
    let id = option.get("id")?.as_str()?.to_owned();
    let category = option
        .get("category")
        .and_then(Value::as_str)
        .map(str::to_owned);
    // Model names are shown in lowercase everywhere.
    if matches_kind(ConfigKind::Model, &id, category.as_deref()) {
        for choice in &mut choices {
            choice.label = choice.label.to_lowercase();
        }
    }
    Some(SelectConfigOption {
        id,
        category,
        current_value: option.get("currentValue")?.as_str()?.to_owned(),
        choices,
    })
}

fn flatten_choices(value: &Value) -> Vec<ConfigChoice> {
    if let Some(options) = value.get("options").and_then(Value::as_array) {
        return options.iter().flat_map(flatten_choices).collect();
    }
    let Some(choice_value) = value.get("value").and_then(Value::as_str) else {
        return Vec::new();
    };
    vec![ConfigChoice {
        value: choice_value.to_owned(),
        label: value
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or(choice_value)
            .to_owned(),
        description: value
            .get("description")
            .and_then(Value::as_str)
            .map(str::to_owned),
    }]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flattens_grouped_select_options() {
        let options = json!({
            "group": "Models",
            "options": [
                {"value": "fast", "name": "Fast"},
                {"value": "deep", "name": "Deep", "description": "More capable"}
            ]
        });
        let choices = flatten_choices(&options);
        assert_eq!(choices.len(), 2);
        assert_eq!(choices[1].value, "deep");
        assert_eq!(choices[1].label, "Deep");
        assert_eq!(choices[1].description.as_deref(), Some("More capable"));
    }

    #[test]
    fn recognizes_standard_model_and_thinking_options() {
        assert!(matches_kind(ConfigKind::Model, "model", Some("model")));
        assert!(matches_kind(
            ConfigKind::Thinking,
            "reasoning_effort",
            Some("thought_level")
        ));
        assert!(matches_kind(ConfigKind::Thinking, "effort", None));
        assert!(!matches_kind(ConfigKind::Model, "mode", None));
    }
}
