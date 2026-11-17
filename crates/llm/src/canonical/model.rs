use serde::{Deserialize, Serialize};

/// Modality types for model input/output
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Modality {
    Text,
    Image,
    Audio,
    Video,
    Pdf,
}

fn deserialize_modalities<'de, D>(deserializer: D) -> Result<Vec<Modality>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let strings: Vec<String> = Vec::deserialize(deserializer)?;
    Ok(strings
        .into_iter()
        .filter_map(|s| serde_json::from_value(serde_json::Value::String(s)).ok())
        .collect())
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Modalities {
    /// Input modalities (e.g., [Text, Image, Pdf])
    #[serde(default, deserialize_with = "deserialize_modalities")]
    pub input: Vec<Modality>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Pricing {
    /// Cost in USD per million input tokens
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<f64>,

    /// Cost in USD per million output tokens
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<f64>,
}

impl Pricing {
    /// True when the entry carries no usable rate signal: an unset or literal-zero value in
    /// either field would make a cost estimate wrong-low rather than merely incomplete.
    pub fn has_no_usable_rate(&self) -> bool {
        fn is_gap(price: Option<f64>) -> bool {
            matches!(price, None | Some(0.0))
        }
        is_gap(self.input) || is_gap(self.output)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Limit {
    /// Maximum context window size in tokens
    pub context: usize,

    /// Maximum output/completion tokens
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<usize>,
}

/// One catalog row: only the fields this app reads (see `scripts/trim-model-registry.py`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanonicalModel {
    /// Model identifier (e.g., "anthropic/claude-3-5-sonnet")
    pub id: String,

    /// Human-readable name (e.g., "Claude Sonnet 3.5 v2")
    pub name: String,

    /// Whether the model supports reasoning/thinking
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<bool>,

    /// Allowed values of the OpenAI-style reasoning effort parameter, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_efforts: Option<Vec<String>>,

    /// Whether the model supports tool calling
    #[serde(default)]
    pub tool_call: bool,

    /// Input modalities (the output ones are not read)
    #[serde(default)]
    pub modalities: Modalities,

    /// Pricing information
    #[serde(default)]
    pub cost: Pricing,

    /// Token limits
    #[serde(default)]
    pub limit: Limit,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pricing(input: Option<f64>, output: Option<f64>) -> Pricing {
        Pricing { input, output }
    }

    #[test]
    fn has_no_usable_rate_covers_absent_zero_and_asymmetric_prices() {
        assert!(pricing(None, None).has_no_usable_rate());
        assert!(pricing(Some(0.0), Some(0.0)).has_no_usable_rate());
        // A half-zero rate would bill one side at nothing, so it is not usable either.
        assert!(pricing(Some(0.0), Some(5.0)).has_no_usable_rate());
        assert!(pricing(Some(5.0), Some(0.0)).has_no_usable_rate());
        assert!(pricing(None, Some(5.0)).has_no_usable_rate());

        assert!(!pricing(Some(1.25), Some(10.0)).has_no_usable_rate());
    }
}
