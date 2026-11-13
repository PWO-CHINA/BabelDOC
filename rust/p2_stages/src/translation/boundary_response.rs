use serde::Deserialize;
use std::collections::{HashMap, HashSet};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Response {
    outputs: Vec<Item>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Item {
    id: String,
    text: String,
}

pub fn parse(response: &str, expected: &[String]) -> Result<HashMap<String, String>, String> {
    let expected = expected.iter().map(String::as_str).collect::<HashSet<_>>();
    let response: Response = serde_json::from_str(response)
        .map_err(|_| "response must contain outputs with stable string id and text fields")?;
    let mut outputs = HashMap::new();
    for item in response.outputs {
        if !expected.contains(item.id.as_str()) {
            return Err(format!("unknown response item id: {}", item.id));
        }
        if outputs.insert(item.id.clone(), item.text).is_some() {
            return Err(format!("duplicate response item id: {}", item.id));
        }
    }
    Ok(outputs)
}
