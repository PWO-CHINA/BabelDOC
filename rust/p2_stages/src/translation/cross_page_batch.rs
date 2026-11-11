use super::cross_page_sentence::{self, SentencePlan};
use super::ordinary_translation::{self, PlaceholderRegex};
use serde::de::{self, Deserialize, Deserializer, MapAccess, Visitor};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BatchItem {
    pub id: usize,
    pub input: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BatchFailure {
    Cancelled,
    RequestFailed,
    InvalidResponse,
}

pub enum BatchResponse<'a> {
    Json(&'a str),
    Cancelled,
    RequestFailed,
}

pub fn request(plan: &SentencePlan) -> [BatchItem; 3] {
    [
        BatchItem { id: 0, input: plan.left_prefix.text.clone() },
        BatchItem { id: 1, input: plan.sentence.clone() },
        BatchItem { id: 2, input: plan.right_suffix.text.clone() },
    ]
}

struct BatchOutput {
    id: usize,
    output: String,
}

impl<'de> Deserialize<'de> for BatchOutput {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct OutputVisitor;
        impl<'de> Visitor<'de> for OutputVisitor {
            type Value = BatchOutput;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("an object containing id and output")
            }

            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
                let mut id = None;
                let mut output = None;
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "id" => {
                            if id.is_some() { return Err(de::Error::duplicate_field("id")); }
                            id = Some(map.next_value()?);
                        }
                        "output" => {
                            if output.is_some() { return Err(de::Error::duplicate_field("output")); }
                            output = Some(map.next_value()?);
                        }
                        _ => return Err(de::Error::unknown_field(&key, &["id", "output"])),
                    }
                }
                Ok(BatchOutput {
                    id: id.ok_or_else(|| de::Error::missing_field("id"))?,
                    output: output.ok_or_else(|| de::Error::missing_field("output"))?,
                })
            }
        }
        deserializer.deserialize_map(OutputVisitor)
    }
}

pub fn candidate<R: PlaceholderRegex>(
    plan: &SentencePlan, response: BatchResponse<'_>, patterns: &[u64], regex: &mut R,
) -> Result<Result<(String, String), BatchFailure>, R::Error> {
    let json = match response {
        BatchResponse::Cancelled => return Ok(Err(BatchFailure::Cancelled)),
        BatchResponse::RequestFailed => return Ok(Err(BatchFailure::RequestFailed)),
        BatchResponse::Json(json) => json,
    };
    let Ok(items) = serde_json::from_str::<Vec<BatchOutput>>(json) else {
        return Ok(Err(BatchFailure::InvalidResponse));
    };
    if items.len() != 3 { return Ok(Err(BatchFailure::InvalidResponse)); }
    let mut outputs: [Option<String>; 3] = [None, None, None];
    for item in items {
        let BatchOutput { id, output } = item;
        if id >= outputs.len() || outputs[id].is_some() { return Ok(Err(BatchFailure::InvalidResponse)); }
        outputs[id] = Some(ordinary_translation::normalize_response_punctuation(&output));
    }
    let [Some(left), Some(sentence), Some(right)] = outputs else { return Ok(Err(BatchFailure::InvalidResponse)) };
    Ok(cross_page_sentence::assemble(plan, &left, &sentence, &right, patterns, regex)?
        .ok_or(BatchFailure::InvalidResponse))
}
