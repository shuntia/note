use crate::run::RunResult;
use serde::Serialize;
use std::fmt::Write as _;

#[derive(Debug, Serialize)]
pub struct Report {
    pub label: String,
    pub started_at: String,
    pub git_sha: String,
    pub repeat: u32,
    pub models: Vec<crate::models::Model>,
    pub missing_models: Vec<String>,
    pub results: Vec<RunResult>,
}

fn median(mut v: Vec<u64>) -> u64 {
    if v.is_empty() {
        return 0;
    }
    v.sort_unstable();
    let mid = v.len() / 2;
    if v.len() % 2 == 1 {
        v[mid]
    } else {
        u64::midpoint(v[mid - 1], v[mid])
    }
}

/// One row per case and model; with repeats, medians over the runs that
/// reached the model.
pub fn markdown(r: &Report) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "# Eval `{}` ({}, {})\n", r.label, r.git_sha, r.started_at);
    let _ = writeln!(out, "| case | model | success | rounds | refused | input tok | output tok | status |");
    let _ = writeln!(out, "|---|---|---|---|---|---|---|---|");
    let mut keys: Vec<(String, String)> = Vec::new();
    for x in &r.results {
        let k = (x.case.clone(), x.model.clone());
        if !keys.contains(&k) {
            keys.push(k);
        }
    }
    for (case, model) in keys {
        let runs: Vec<&RunResult> = r.results.iter().filter(|x| x.case == case && x.model == model).collect();
        let reached: Vec<&&RunResult> = runs.iter().filter(|x| x.status != "unavailable").collect();
        let passed = runs.iter().filter(|x| x.success).count();
        let pick = |f: fn(&RunResult) -> u64| median(reached.iter().map(|x| f(x)).collect());
        let statuses: Vec<&str> = runs.iter().map(|x| x.status.as_str()).collect();
        let success = if runs.len() == 1 {
            if runs[0].success { "pass".to_string() } else { "fail".to_string() }
        } else {
            format!("{passed}/{}", runs.len())
        };
        if reached.is_empty() {
            let _ = writeln!(out, "| {case} | {model} | — | — | — | — | — | {} |", statuses.join(", "));
            continue;
        }
        let _ = writeln!(
            out,
            "| {case} | {model} | {success} | {} | {} | {} | {} | {} |",
            pick(|x| x.score.rounds),
            pick(|x| x.score.refused),
            pick(|x| x.score.prompt_tokens),
            pick(|x| x.score.completion_tokens),
            statuses.join(", "),
        );
    }
    let failed: Vec<&RunResult> = r.results.iter().filter(|x| !x.success).collect();
    if !failed.is_empty() {
        let _ = writeln!(out, "\n## Why runs failed\n");
        for x in failed {
            let mut why: Vec<String> = x.failures.clone();
            if let Some(e) = &x.error {
                why.insert(0, e.clone());
            }
            let _ = writeln!(out, "- {} / {} #{}: {}", x.case, x.model, x.repeat, why.join("; ").replace('|', "/"));
        }
    }
    if !r.missing_models.is_empty() {
        let _ = writeln!(out, "\nNot in the OpenRouter catalogue: {}", r.missing_models.join(", "));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn medians_take_the_middle() {
        assert_eq!(median(vec![]), 0);
        assert_eq!(median(vec![5, 1, 3]), 3);
        assert_eq!(median(vec![4, 1, 3, 2]), 2);
    }
}
