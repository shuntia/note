mod cases;
mod checks;
mod client;
mod models;
mod report;
mod run;
mod scratch;
mod score;
mod snapshot;

use anyhow::{bail, ensure, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

const USAGE: &str = "usage: note-eval --label <label> [--case <name>]... [--model <short|id>]... [--repeat <n>] \
                     [--key-file <path>] [--server-bin <path>]";
const KEY_ENV: &str = "NOTE_EVAL_KEY_FILE";
const DEFAULT_KEY: &str = ".config/note-eval/openrouter-free.key";

struct Args {
    label: String,
    cases: Vec<String>,
    models: Vec<String>,
    repeat: u32,
    key_file: Option<PathBuf>,
    server_bin: Option<PathBuf>,
}

fn parse_args(raw: &[String]) -> Result<Args> {
    let mut a = Args { label: String::new(), cases: Vec::new(), models: Vec::new(), repeat: 1, key_file: None, server_bin: None };
    let mut it = raw.iter();
    while let Some(flag) = it.next() {
        let mut value = || it.next().cloned().with_context(|| format!("{flag} takes a value\n{USAGE}"));
        match flag.as_str() {
            "--label" => a.label = value()?,
            "--case" => a.cases.push(value()?),
            "--model" => a.models.push(value()?),
            "--repeat" => a.repeat = value()?.parse().context("--repeat takes a number")?,
            "--key-file" => a.key_file = Some(PathBuf::from(value()?)),
            "--server-bin" => a.server_bin = Some(PathBuf::from(value()?)),
            "-h" | "--help" => bail!("{USAGE}"),
            other => bail!("unknown argument {other:?}\n{USAGE}"),
        }
    }
    ensure!(!a.label.is_empty(), "--label is required\n{USAGE}");
    ensure!(a.label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'), "--label takes letters, digits, - and _");
    ensure!(a.repeat >= 1, "--repeat must be at least 1");
    for c in &a.cases {
        ensure!(cases::NAMES.contains(&c.as_str()), "unknown case {c:?}; cases: {}", cases::NAMES.join(", "));
    }
    Ok(a)
}

/// The key file: present, non-empty, and outside the repo so it cannot be
/// committed. Its content is never read here.
fn key_file(arg: Option<PathBuf>, repo: &Path) -> Result<PathBuf> {
    let path = arg
        .or_else(|| std::env::var_os(KEY_ENV).map(PathBuf::from))
        .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(DEFAULT_KEY)))
        .context("no key file: pass --key-file or set NOTE_EVAL_KEY_FILE")?;
    let path = path.canonicalize().with_context(|| format!("key file {} is missing", path.display()))?;
    ensure!(std::fs::metadata(&path)?.len() > 0, "key file {} is empty", path.display());
    ensure!(!path.starts_with(repo), "the key file must live outside the repo");
    Ok(path)
}

fn build_server(repo: &Path) -> Result<PathBuf> {
    eprintln!("building note-server…");
    let status = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
        .args(["build", "-p", "note-server"])
        .current_dir(repo)
        .status()
        .context("running cargo build")?;
    ensure!(status.success(), "cargo build -p note-server failed");
    let target = std::env::var_os("CARGO_TARGET_DIR").map_or_else(|| repo.join("target"), |t| repo.join(t));
    let bin = target.join("debug/note-server");
    ensure!(bin.is_file(), "no binary at {}", bin.display());
    Ok(bin)
}

fn git_sha(repo: &Path) -> String {
    Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(repo)
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".into())
}

fn write_results(results_dir: &Path, stem: &str, r: &report::Report) -> Result<()> {
    std::fs::write(results_dir.join(format!("{stem}.json")), serde_json::to_string_pretty(r)?)?;
    std::fs::write(results_dir.join(format!("{stem}.md")), report::markdown(r))?;
    Ok(())
}

fn main() -> Result<()> {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let args = parse_args(&raw)?;
    let eval_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let repo = eval_dir.parent().context("eval/ has no parent")?.canonicalize()?;

    let file = models::load(&eval_dir.join("models.toml"))?;
    let mut chosen: Vec<models::Model> = file
        .models
        .iter()
        .filter(|m| args.models.is_empty() || args.models.iter().any(|a| *a == m.short || *a == m.id))
        .cloned()
        .collect();
    ensure!(!chosen.is_empty(), "no configured model matches {:?}", args.models);
    let missing = models::check_prices(&file.models, &models::fetch_catalogue()?)?;
    chosen.retain(|m| !missing.contains(&m.id));
    let served: Vec<models::Model> = file.models.iter().filter(|m| !missing.contains(&m.id)).cloned().collect();

    let key = key_file(args.key_file.clone(), &repo)?;
    let server_bin = match &args.server_bin {
        Some(p) => p.canonicalize().with_context(|| format!("no server binary at {}", p.display()))?,
        None => build_server(&repo)?,
    };
    let case_names: Vec<String> =
        if args.cases.is_empty() { cases::NAMES.iter().map(ToString::to_string).collect() } else { args.cases.clone() };
    let cases_dir = eval_dir.join("cases");
    let cases: Vec<cases::Case> = case_names.iter().map(|n| cases::load(&cases_dir, n)).collect::<Result<_>>()?;

    let started = jiff::Timestamp::now();
    let stamp = started.strftime("%Y%m%dT%H%M%SZ").to_string();
    let stem = format!("{}-{stamp}", args.label);
    let results_dir = eval_dir.join("results");
    let keep_dir = results_dir.join(&stem);
    std::fs::create_dir_all(&keep_dir)?;
    let mut rep = report::Report {
        label: args.label.clone(),
        started_at: started.to_string(),
        git_sha: git_sha(&repo),
        repeat: args.repeat,
        models: chosen.clone(),
        missing_models: missing,
        results: Vec::new(),
    };
    let env = run::Env {
        repo: &repo,
        server_bin: &server_bin,
        key_file: &key,
        models: &served,
        base_url: &file.base_url,
        keep_dir: &keep_dir,
    };
    let mut out_of_reach: Vec<String> = Vec::new();
    for model in &chosen {
        for case in &cases {
            for repeat in 1..=args.repeat {
                if out_of_reach.contains(&model.id) {
                    rep.results.push(skipped(case, model, repeat));
                    continue;
                }
                eprintln!("{} on {} (#{repeat})…", case.name, model.short);
                let (result, gone) = run::run(&env, case, model, repeat);
                eprintln!(
                    "  {}: {} ({} rounds, {} refused, {} in / {} out)",
                    result.status,
                    if result.success { "pass" } else { "fail" },
                    result.score.rounds,
                    result.score.refused,
                    result.score.prompt_tokens,
                    result.score.completion_tokens
                );
                let spent = result.score.cost;
                rep.results.push(result);
                write_results(&results_dir, &stem, &rep)?;
                ensure!(spent == 0.0, "a run reported a cost of {spent}; stopping so nothing more is spent");
                if gone {
                    out_of_reach.push(model.id.clone());
                }
            }
        }
    }
    write_results(&results_dir, &stem, &rep)?;
    print!("{}", report::markdown(&rep));
    eprintln!("results: {}", results_dir.join(format!("{stem}.json")).display());
    Ok(())
}

fn skipped(case: &cases::Case, model: &models::Model, repeat: u32) -> run::RunResult {
    run::RunResult {
        case: case.name.clone(),
        model: model.short.clone(),
        model_id: model.id.clone(),
        repeat,
        status: "unavailable".into(),
        success: false,
        failures: Vec::new(),
        score: score::Score::default(),
        wall_secs: 0,
        waited_secs: 0,
        attempts: 0,
        error: Some("skipped: the provider asked for a wait past the cap earlier in this run".into()),
        fixture: checks::Fixture::default(),
        snapshot: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn repeat_defaults_to_one_and_a_label_is_required() {
        let a = parse_args(&args("--label before --case visual-arts --model gemma")).unwrap();
        assert_eq!((a.repeat, a.cases.len(), a.models.len()), (1, 1, 1));
        assert!(parse_args(&args("--case visual-arts")).is_err());
        assert!(parse_args(&args("--label x --case nope")).is_err());
    }

    #[test]
    fn there_is_no_flag_for_another_server() {
        assert!(parse_args(&args("--label x --server http://127.0.0.1:3271")).is_err());
        assert!(parse_args(&args("--label x --url https://note.shuntia.net")).is_err());
    }

    #[test]
    fn a_key_file_inside_the_repo_is_refused() {
        let repo = tempfile::tempdir().unwrap();
        let repo_path = repo.path().canonicalize().unwrap();
        let inside = repo_path.join("config/openrouter.key");
        std::fs::create_dir_all(inside.parent().unwrap()).unwrap();
        std::fs::write(&inside, "k").unwrap();
        assert!(key_file(Some(inside), &repo_path).is_err());
        let outside = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(outside.path(), "k").unwrap();
        assert!(key_file(Some(outside.path().into()), &repo_path).is_ok());
        let empty = tempfile::NamedTempFile::new().unwrap();
        assert!(key_file(Some(empty.path().into()), &repo_path).is_err());
    }
}
