//! Port of the source runtime's router-study `offline_candidates.py` (SHA-256 `36b2fe30...`, see lib.rs).
//! A row passes when the picked cell passed every seed it has. Rows labelled
//! `routed_by_forget_cue` or `uncertain` leave the denominator (offline_candidates.py:23).
//! This proves the port is faithful; the labels have no Granite 4.2 cells, so it proves nothing
//! about how `auto` sizes chat.

use serde_json::Value;

const PATH_RULE: [(&str, &str); 4] = [
    ("memory.decide", "granite41-off"),
    ("tools", "granite41-off"),
    ("approval-flow", "flash-off"),
    ("d3-route-delegate", "flash-off"),
];

fn ok(cell: Option<&Value>) -> bool {
    cell.is_some_and(|c| {
        let seeds = c["seeds"].as_u64().unwrap_or(0);
        seeds > 0 && c["pass"].as_u64() == Some(seeds)
    })
}

fn path_rule(row: &Value, default: &'static str) -> &'static str {
    let node = row["node"].as_str().unwrap_or("");
    PATH_RULE
        .iter()
        .find(|(n, _)| *n == node)
        .map_or(default, |(_, c)| c)
}

/// (scored rows, pass %, answered locally %), rounded to one decimal like the script.
fn run(rows: &[Value], pick: impl Fn(&Value) -> &'static str) -> (usize, f64, f64) {
    let (mut n, mut pass, mut local) = (0, 0, 0);
    for row in rows {
        if matches!(
            row["label"].as_str(),
            Some("routed_by_forget_cue" | "uncertain")
        ) {
            continue;
        }
        n += 1;
        let cell = pick(row);
        if ok(row["cells"].get(cell)) {
            pass += 1;
            local += usize::from(cell.starts_with("granite"));
        }
    }
    let pct = |k: usize| (1000.0 * k as f64 / n as f64).round() / 10.0;
    (n, pct(pass), pct(local))
}

/// The four candidates whose figures the study reported: all Granite, all Flash, per-path rules with
/// chat on Flash, and the same rules with chat on Granite.
fn candidates(rows: &[Value]) -> [(usize, f64, f64); 4] {
    [
        run(rows, |_| "granite41-off"),
        run(rows, |_| "flash-off"),
        run(rows, |r| path_rule(r, "flash-off")),
        run(rows, |r| path_rule(r, "granite41-off")),
    ]
}

#[test]
fn synthetic_fixture() {
    let cell = |pass: u64| serde_json::json!({"pass": pass, "seeds": 3});
    let row = |node: &str, label: &str, granite: u64, flash: u64| {
        serde_json::json!({"node": node, "label": label,
            "cells": {"granite41-off": cell(granite), "flash-off": cell(flash)}})
    };
    let rows = [
        row("chat", "none", 3, 3),
        row("chat", "none", 0, 3),
        row("tools", "none", 3, 0),
        row("approval-flow", "none", 0, 3),
        row("chat", "none", 2, 0),
        row("chat", "uncertain", 3, 3),
    ];
    assert_eq!(
        candidates(&rows),
        [
            (5, 40.0, 40.0),
            (5, 60.0, 0.0),
            (5, 80.0, 20.0),
            (5, 60.0, 40.0)
        ]
    );
}

/// Reproduces the router study's published figures from its private labels:
/// `ROUTER_LABELS=.../router-study/step1/labels.jsonl cargo test -p localfleet-route -- --ignored`
#[test]
#[ignore = "needs the router study's private labels in ROUTER_LABELS"]
fn reproduces_router_study_step2() {
    use sha2::{Digest, Sha256};
    let path = std::env::var("ROUTER_LABELS")
        .expect("set ROUTER_LABELS to the router study's step1/labels.jsonl");
    let text = std::fs::read(&path).expect("ROUTER_LABELS is not readable");
    let digest: String = Sha256::digest(&text)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(
        digest, "54e0dc1ff8e71b6c9b4d14ba035885d822f12e9ab27899896804f6859225f70b",
        "labels.jsonl changed since the figures were published"
    );
    let rows: Vec<Value> = String::from_utf8(text)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(rows.len(), 311);
    assert_eq!(
        candidates(&rows),
        [
            (305, 64.3, 64.3),
            (305, 81.0, 0.0),
            (305, 76.7, 19.0),
            (305, 67.5, 61.6)
        ]
    );
}
