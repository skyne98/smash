//! Model list from models.dev: which models exist, what they cost, how big
//! their context is, whether and how they think. Cached on disk, refreshed
//! in the background; a missing network never blocks startup.

use std::path::Path;
use std::time::{Duration, SystemTime};

use genai::resolver::{AuthData, Endpoint};
use genai::{ModelIden, ServiceTarget};
use serde::Deserialize;
use serde_json::Value;

use crate::{AdapterKind, Effort, Model, Price};

pub const URL: &str = "https://models.dev/api.json";
/// Older caches are refreshed (in the background) on the next start.
pub const MAX_AGE: Duration = Duration::from_secs(24 * 3600);

/// One usable model.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub provider: String,
    pub provider_name: String,
    pub id: String,
    pub name: String,
    /// API key variables; the model is usable when one is set.
    pub env: Vec<String>,
    /// OpenAI-compatible base URL, for providers without a native adapter.
    pub api: Option<String>,
    /// Thinking levels the model accepts, weakest first; empty = no thinking control.
    pub efforts: Vec<Effort>,
    pub context: u64,
    pub output: u64,
    pub price: Option<Price>,
    /// Earlier reasoning must be sent back as `reasoning_content`.
    pub passback: bool,
}

impl Entry {
    /// `provider/model`, the id users type and settings store.
    pub fn key(&self) -> String {
        format!("{}/{}", self.provider, self.id)
    }

    /// The first configured API key variable, if any.
    pub fn key_env(&self) -> Option<&str> {
        self.env
            .iter()
            .find(|name| std::env::var_os(name).is_some_and(|value| !value.is_empty()))
            .map(String::as_str)
    }

    pub fn available(&self) -> bool {
        self.key_env().is_some()
    }

    /// A ready-to-call model: native genai adapter where one exists,
    /// otherwise the provider's OpenAI-compatible endpoint.
    pub fn model(&self) -> Option<Model> {
        let native = match self.provider.as_str() {
            "deepseek" => Some(AdapterKind::DeepSeek),
            "anthropic" => Some(AdapterKind::Anthropic),
            "openai" => Some(AdapterKind::OpenAI),
            _ => None,
        };
        let mut model = match (native, &self.api) {
            (Some(kind), _) => Model::any(kind, self.id.clone()),
            (None, Some(api)) => {
                let base = if api.ends_with('/') {
                    api.clone()
                } else {
                    format!("{api}/")
                };
                let target = ServiceTarget {
                    endpoint: Endpoint::from_owned(base),
                    auth: AuthData::from_env(
                        self.key_env().or(self.env.first().map(String::as_str))?,
                    ),
                    model: ModelIden::new(AdapterKind::OpenAI, self.id.clone()),
                };
                Model::from_target(target)
            }
            (None, None) => return None,
        };
        model.reasoning_passback = self.passback || self.provider == "deepseek";
        model.price = self.price;
        Some(model)
    }

    /// Next thinking level after `current`, wrapping; None when not adjustable.
    pub fn next_effort(&self, current: Option<Effort>) -> Option<Effort> {
        if self.efforts.is_empty() {
            return None;
        }
        let at = current.and_then(|effort| self.efforts.iter().position(|e| *e == effort));
        Some(match at {
            Some(index) => self.efforts[(index + 1) % self.efforts.len()],
            None => self.efforts[0],
        })
    }

    /// The level used when the user never picked one: the provider's own
    /// default where known (high), else the strongest below max.
    pub fn default_effort(&self) -> Option<Effort> {
        if self.efforts.contains(&Effort::High) {
            return Some(Effort::High);
        }
        self.efforts
            .iter()
            .rev()
            .find(|e| **e != Effort::Max)
            .copied()
    }
}

#[derive(Clone, Debug, Default)]
pub struct Catalog {
    pub entries: Vec<Entry>,
}

#[derive(Deserialize)]
struct RawProvider {
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    env: Vec<String>,
    api: Option<String>,
    #[serde(default)]
    models: serde_json::Map<String, Value>,
}

impl Catalog {
    pub fn parse(json: &str) -> Result<Self, String> {
        let providers: serde_json::Map<String, Value> =
            serde_json::from_str(json).map_err(|err| err.to_string())?;
        let mut entries = Vec::new();
        for (key, value) in providers {
            let Ok(provider) = serde_json::from_value::<RawProvider>(value) else {
                continue;
            };
            let id = if provider.id.is_empty() {
                key
            } else {
                provider.id
            };
            let native = matches!(id.as_str(), "deepseek" | "anthropic" | "openai");
            if !native && provider.api.is_none() {
                continue;
            }
            for (model_id, model) in &provider.models {
                // Agents need tools; skip models that can't call them.
                if model["tool_call"].as_bool() != Some(true) {
                    continue;
                }
                entries.push(Entry {
                    provider: id.clone(),
                    provider_name: provider.name.clone(),
                    id: model["id"].as_str().unwrap_or(model_id).to_string(),
                    name: model["name"].as_str().unwrap_or(model_id).to_string(),
                    env: provider.env.clone(),
                    api: provider.api.clone(),
                    efforts: efforts(model),
                    context: model["limit"]["context"].as_u64().unwrap_or(0),
                    output: model["limit"]["output"].as_u64().unwrap_or(0),
                    price: price(&model["cost"]),
                    passback: model["interleaved"]["field"].as_str() == Some("reasoning_content"),
                });
            }
        }
        entries.sort_by(|a, b| (&a.provider, &a.id).cmp(&(&b.provider, &b.id)));
        Ok(Self { entries })
    }

    /// The cached list, if there is one.
    pub fn load(cache: &Path) -> Option<Self> {
        Self::parse(&std::fs::read_to_string(cache).ok()?).ok()
    }

    pub fn stale(cache: &Path) -> bool {
        let modified = std::fs::metadata(cache).and_then(|meta| meta.modified());
        modified.map_or(true, |at| {
            SystemTime::now().duration_since(at).unwrap_or_default() > MAX_AGE
        })
    }

    /// Download, store, parse. The cache is replaced only by a list that parses.
    pub async fn refresh(cache: &Path) -> Result<Self, String> {
        crate::tls();
        let body = reqwest::get(URL)
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(|err| err.to_string())?
            .text()
            .await
            .map_err(|err| err.to_string())?;
        let catalog = Self::parse(&body)?;
        if let Some(dir) = cache.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let partial = cache.with_extension("part");
        std::fs::write(&partial, &body).map_err(|err| err.to_string())?;
        std::fs::rename(&partial, cache).map_err(|err| err.to_string())?;
        Ok(catalog)
    }

    /// By `provider/model`, or by bare model id (first usable match wins).
    pub fn find(&self, key: &str) -> Option<&Entry> {
        match key.split_once('/') {
            Some((provider, id)) => self
                .entries
                .iter()
                .find(|entry| entry.provider == provider && entry.id == id),
            None => self
                .entries
                .iter()
                .filter(|entry| entry.id == key)
                .max_by_key(|entry| entry.available()),
        }
    }

    /// Models whose API key is set: what a picker should offer.
    pub fn available(&self) -> Vec<&Entry> {
        self.entries
            .iter()
            .filter(|entry| entry.available())
            .collect()
    }
}

fn efforts(model: &Value) -> Vec<Effort> {
    if model["reasoning"].as_bool() != Some(true) {
        return Vec::new();
    }
    let options = model["reasoning_options"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let mut levels = Vec::new();
    if options.iter().any(|o| o["type"] == "toggle") {
        levels.push(Effort::Off);
    }
    for option in &options {
        for value in option["values"].as_array().into_iter().flatten() {
            if let Some(effort) = value.as_str().and_then(Effort::parse)
                && !levels.contains(&effort)
            {
                levels.push(effort);
            }
        }
    }
    levels
}

fn price(cost: &Value) -> Option<Price> {
    let input = cost["input"].as_f64()?;
    Some(Price {
        input,
        output: cost["output"].as_f64().unwrap_or(0.0),
        cache_read: cost["cache_read"].as_f64().unwrap_or(input),
        cache_write: cost["cache_write"].as_f64().unwrap_or(input),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
      "deepseek": { "id": "deepseek", "name": "DeepSeek", "env": ["SMASH_TEST_NO_SUCH_KEY"],
        "api": "https://api.deepseek.com", "models": {
          "deepseek-flash": { "id": "deepseek-flash", "name": "DeepSeek V4.1 Flash", "reasoning": true,
            "reasoning_options": [{"type":"toggle"},{"type":"effort","values":["low","high","max"]}],
            "tool_call": true, "interleaved": {"field": "reasoning_content"},
            "limit": {"context": 1000000, "output": 393216},
            "cost": {"input": 0.15, "output": 0.6, "cache_read": 0.003} },
          "no-tools": { "id": "no-tools", "tool_call": false } } },
      "lonely": { "id": "lonely", "env": ["X"], "models": { "m": { "tool_call": true } } },
      "compat": { "id": "compat", "name": "Compat", "env": ["PATH"], "api": "https://x.test/v1",
        "models": { "m1": { "id": "m1", "tool_call": true, "reasoning": false } } }
    }"#;

    #[test]
    fn parses_what_we_can_call() {
        let catalog = Catalog::parse(SAMPLE).unwrap();
        let keys: Vec<String> = catalog.entries.iter().map(Entry::key).collect();
        assert_eq!(
            keys,
            ["compat/m1", "deepseek/deepseek-flash"],
            "no tools / no endpoint skipped"
        );
        let flash = catalog.find("deepseek-flash").unwrap();
        assert_eq!(
            flash.efforts,
            [Effort::Off, Effort::Low, Effort::High, Effort::Max]
        );
        assert_eq!(flash.default_effort(), Some(Effort::High));
        assert_eq!(flash.next_effort(Some(Effort::Max)), Some(Effort::Off));
        assert_eq!(flash.context, 1_000_000);
        assert!(flash.passback);
        assert_eq!(
            flash.price.unwrap().cache_write,
            0.15,
            "missing price falls back to input"
        );
        assert!(!flash.available(), "key not set");

        let compat = catalog.find("compat/m1").unwrap();
        assert!(compat.available(), "PATH is always set");
        assert!(compat.efforts.is_empty());
        assert_eq!(compat.next_effort(None), None);
        assert!(compat.model().is_some());
        assert_eq!(catalog.available().len(), 1);
    }
}
