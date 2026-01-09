use std::collections::BTreeMap;

use toml_edit::{Array, DocumentMut, Item, Table, Value as TomlValue};

use crate::core::{utils, McpServer, Profile};
use crate::error::{Error, Result};
use crate::harness::{AgentPaths, ApplyReport};

pub fn apply(
    _profile: &Profile,
    paths: &AgentPaths,
    mcp_servers: &BTreeMap<String, McpServer>,
    model: Option<&str>,
    _report: &mut ApplyReport,
) -> Result<()> {
    let mut doc = load_toml(&paths.config_file)?;

    if let Some(model) = model {
        doc["model"] = toml_edit::value(model);
    }

    let mut mcp_table = Table::new();
    for (name, server) in mcp_servers {
        let mut server_table = Table::new();
        if let Some(command) = &server.command {
            server_table["command"] = toml_edit::value(command);
        }
        if let Some(args) = &server.args {
            let mut array = Array::new();
            for arg in args {
                array.push(arg.as_str());
            }
            server_table["args"] = Item::Value(TomlValue::Array(array));
        }
        if let Some(url) = &server.url {
            server_table["url"] = toml_edit::value(url);
        }
        if let Some(env) = &server.env {
            let mut env_table = Table::new();
            for (key, value) in env {
                env_table[key] = toml_edit::value(value);
            }
            server_table["env"] = Item::Table(env_table);
        }
        if let Some(headers) = &server.headers {
            let mut headers_table = Table::new();
            for (key, value) in headers {
                headers_table[key] = toml_edit::value(value);
            }
            server_table["headers"] = Item::Table(headers_table);
        }
        if let Some(enabled) = server.enabled {
            server_table["enabled"] = toml_edit::value(enabled);
        }

        mcp_table.insert(name, Item::Table(server_table));
    }

    doc["mcp_servers"] = Item::Table(mcp_table);

    utils::write_string(&paths.config_file, &format!("{}\n", doc.to_string()))
}

fn load_toml(path: &std::path::Path) -> Result<DocumentMut> {
    if utils::exists(path) {
        let raw = utils::read_to_string(path)?;
        raw.parse::<DocumentMut>().map_err(|err| {
            Error::InvalidInput(format!("could not parse {}: {}", path.display(), err))
        })
    } else {
        Ok(DocumentMut::new())
    }
}
