//! Internal YAML boundary.
//!
//! The current backend is deprecated upstream. Keeping it private prevents
//! backend-specific APIs from spreading while we evaluate maintained parsers
//! against the profile and sub-agent compatibility tests.

pub use yaml_backend::{from_str, to_string, to_value, Mapping, Value};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compatibility_parses_nested_frontmatter_values() {
        let value: Value =
            from_str("model:\n  default: inherit\ntools:\n  read: allow\nmaxTurns: 10\n").unwrap();

        assert_eq!(value["model"]["default"].as_str(), Some("inherit"));
        assert_eq!(value["tools"]["read"].as_str(), Some("allow"));
        assert_eq!(value["maxTurns"].as_u64(), Some(10));
    }

    #[test]
    fn compatibility_round_trips_dynamic_mapping_values() {
        let mut mapping = Mapping::new();
        mapping.insert(Value::String("enabled".into()), Value::Bool(true));
        mapping.insert(
            Value::String("items".into()),
            Value::Sequence(vec![Value::String("one".into())]),
        );

        let text = to_string(&mapping).unwrap();
        let parsed: Mapping = from_str(&text).unwrap();

        assert_eq!(parsed, mapping);
    }
}
