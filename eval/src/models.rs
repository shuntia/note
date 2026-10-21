use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

pub const OPENROUTER: &str = "https://openrouter.ai/api/v1";
const FREE_SUFFIX: &str = ":free";

#[derive(Debug, Clone, Deserialize)]
pub struct ModelsFile {
    pub base_url: String,
    pub models: Vec<Model>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct Model {
    pub short: String,
    pub id: String,
    pub name: String,
}

pub fn load(path: &Path) -> Result<ModelsFile> {
    let raw = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let file: ModelsFile = toml::from_str(&raw).with_context(|| format!("parsing {}", path.display()))?;
    check_file(&file)?;
    Ok(file)
}

/// Refuses a models file that could reach anything but `OpenRouter`'s free tier.
pub fn check_file(file: &ModelsFile) -> Result<()> {
    if file.base_url.trim_end_matches('/') != OPENROUTER {
        bail!("base_url must be {OPENROUTER}, not {:?}", file.base_url);
    }
    if file.models.is_empty() {
        bail!("models.toml names no models");
    }
    for m in &file.models {
        if !m.id.ends_with(FREE_SUFFIX) {
            bail!("model {:?} is not a {FREE_SUFFIX} model; the eval runs on free models only", m.id);
        }
    }
    Ok(())
}

pub fn fetch_catalogue() -> Result<serde_json::Value> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(30)))
        .build()
        .into();
    let mut resp = agent.get(&format!("{OPENROUTER}/models")).call().context("fetching the OpenRouter catalogue")?;
    resp.body_mut()
        .with_config()
        .limit(64 * 1024 * 1024)
        .read_json()
        .context("reading the OpenRouter catalogue")
}

/// Which configured models the catalogue lists. Any listed price that is not
/// zero refuses the whole run; a model the catalogue lacks is reported missing.
pub fn check_prices(models: &[Model], catalogue: &serde_json::Value) -> Result<Vec<String>> {
    let listed = catalogue["data"].as_array().context("catalogue has no data array")?;
    let mut missing = Vec::new();
    for m in models {
        let Some(entry) = listed.iter().find(|e| e["id"].as_str() == Some(m.id.as_str())) else {
            missing.push(m.id.clone());
            continue;
        };
        let pricing = entry["pricing"].as_object().with_context(|| format!("{} has no pricing", m.id))?;
        for (field, price) in pricing {
            let zero = match price {
                serde_json::Value::String(s) => s.trim().parse::<f64>().is_ok_and(|p| p == 0.0),
                serde_json::Value::Number(n) => n.as_f64() == Some(0.0),
                serde_json::Value::Null => true,
                _ => false,
            };
            if !zero {
                bail!("{} lists a {field} price of {price}; refusing to run a model that can cost money", m.id);
            }
        }
    }
    Ok(missing)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(id: &str) -> Model {
        Model { short: "m".into(), id: id.into(), name: "M".into() }
    }

    fn file(ids: &[&str]) -> ModelsFile {
        ModelsFile { base_url: OPENROUTER.into(), models: ids.iter().map(|i| model(i)).collect() }
    }

    #[test]
    fn a_model_without_the_free_suffix_is_refused() {
        assert!(check_file(&file(&["a/b:free", "c/d"])).is_err());
        assert!(check_file(&file(&["a/b:free"])).is_ok());
    }

    #[test]
    fn another_endpoint_is_refused() {
        let mut f = file(&["a/b:free"]);
        f.base_url = "https://api.openai.com/v1".into();
        assert!(check_file(&f).is_err());
    }

    #[test]
    fn the_shipped_models_file_is_free_only() {
        let f = load(&Path::new(env!("CARGO_MANIFEST_DIR")).join("models.toml")).unwrap();
        assert_eq!(f.models.len(), 3);
    }

    #[test]
    fn a_priced_model_refuses_the_run() {
        let cat = serde_json::json!({ "data": [
            { "id": "a/b:free", "pricing": { "prompt": "0", "completion": "0", "request": "0" } },
            { "id": "c/d:free", "pricing": { "prompt": "0", "completion": "0.000002" } },
        ]});
        assert!(check_prices(&[model("a/b:free")], &cat).unwrap().is_empty());
        assert!(check_prices(&[model("a/b:free"), model("c/d:free")], &cat).is_err());
    }

    #[test]
    fn a_dynamic_or_unreadable_price_refuses_the_run() {
        let cat = serde_json::json!({ "data": [
            { "id": "a/b:free", "pricing": { "prompt": "-1", "completion": "0" } },
            { "id": "c/d:free", "pricing": { "prompt": "n/a", "completion": "0" } },
        ]});
        assert!(check_prices(&[model("a/b:free")], &cat).is_err());
        assert!(check_prices(&[model("c/d:free")], &cat).is_err());
    }

    #[test]
    fn an_unlisted_model_is_reported_missing() {
        let cat = serde_json::json!({ "data": [] });
        assert_eq!(check_prices(&[model("a/b:free")], &cat).unwrap(), vec!["a/b:free".to_string()]);
    }
}
