use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct DsdSourceRule {
    pub source_rate: u32,
    pub filter_type: String,
    pub output_mode: String,
}
